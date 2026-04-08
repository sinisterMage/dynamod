use std::collections::HashMap;
use std::ffi::CString;
use std::io::{BufRead, BufReader};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::path::Path;

use nix::sys::signal::Signal;
use nix::unistd::{self, ForkResult, Pid};

use crate::config::service::ServiceDef;

/// Path to the log socket (must match dynamod-logd).
const LOG_SOCKET_PATH: &str = "/run/dynamod/log.sock";

/// Information about a successfully spawned service process.
#[derive(Debug)]
pub struct SpawnedProcess {
    pub pid: Pid,
    pub pidfd: Option<RawFd>,
    /// Read end of the readiness fd pipe (for ReadinessType::Fd).
    /// The write end was passed to the child as DYNAMOD_READY_FD.
    pub ready_fd: Option<RawFd>,
}

/// Errors that can occur during process spawning.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("exec command is empty")]
    EmptyExec,
    #[error("fork failed: {0}")]
    Fork(nix::Error),
    #[error("exec failed: {0}")]
    Exec(nix::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("pidfd_open failed: {0}")]
    PidfdOpen(nix::Error),
}

/// Spawn a service process based on its definition.
/// Returns the PID and pidfd of the spawned process.
pub fn spawn_service(def: &ServiceDef) -> Result<SpawnedProcess, SpawnError> {
    if def.service.exec.is_empty() {
        return Err(SpawnError::EmptyExec);
    }

    let program = CString::new(def.service.exec[0].as_bytes())
        .map_err(|_| SpawnError::InvalidPath(def.service.exec[0].clone()))?;

    let args: Vec<CString> = def
        .service
        .exec
        .iter()
        .map(|a| {
            CString::new(a.as_bytes())
                .map_err(|_| SpawnError::InvalidPath(a.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Build environment
    let mut env_map: HashMap<String, String> = std::env::vars().collect();

    // Load environment file if specified
    if let Some(ref env_file) = def.service.environment_file {
        if let Ok(contents) = std::fs::read_to_string(env_file) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    env_map.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
        }
    }

    // Apply inline environment (overrides env file)
    for (k, v) in &def.service.environment {
        env_map.insert(k.clone(), v.clone());
    }

    // Create ready-fd pipe for ReadinessType::Fd services
    use crate::config::service::ReadinessType;
    let ready_pipe = if def.readiness.readiness_type == ReadinessType::Fd {
        create_pipe()
    } else {
        None
    };

    // Pass the write end fd to the child via DYNAMOD_READY_FD
    if let Some((_, write_end)) = ready_pipe {
        env_map.insert("DYNAMOD_READY_FD".to_string(), write_end.to_string());
    }

    // Set NOTIFY_SOCKET for sd_notify-compatible services
    if def.readiness.readiness_type == ReadinessType::Notify {
        let notify_path = format!("/run/dynamod/notify-{}.sock", def.service.name);
        env_map.insert("NOTIFY_SOCKET".to_string(), notify_path);
    }

    let env: Vec<CString> = env_map
        .iter()
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();

    // Create a pipe for capturing stdout/stderr
    let log_pipe = create_pipe();

    // Fork
    let fork_result = unsafe { unistd::fork() }.map_err(SpawnError::Fork)?;

    match fork_result {
        ForkResult::Child => {
            // === Child process ===

            // Redirect stdin to /dev/null so services don't inherit
            // the console fd and steal keystrokes from login/greeter
            // processes (agetty, greetd) that open their own TTY.
            unsafe {
                let devnull = libc::open(
                    b"/dev/null\0".as_ptr() as *const libc::c_char,
                    libc::O_RDONLY,
                );
                if devnull >= 0 {
                    libc::dup2(devnull, 0);
                    if devnull > 0 {
                        libc::close(devnull);
                    }
                }
            }

            // Redirect stdout/stderr to the log pipe
            if let Some((_, write_end)) = log_pipe {
                unsafe {
                    if libc::dup2(write_end, 1) < 0 || libc::dup2(write_end, 2) < 0 {
                        libc::_exit(126);
                    }
                    if write_end > 2 {
                        libc::close(write_end);
                    }
                }
            }
            // Close read end in child
            if let Some((read_end, _)) = log_pipe {
                unsafe { libc::close(read_end); }
            }

            // Clear CLOEXEC on the ready-fd write end so it survives exec
            if let Some((read_end, write_end)) = ready_pipe {
                unsafe {
                    libc::close(read_end);
                    let flags = libc::fcntl(write_end, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(write_end, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
            }

            // Apply namespace isolation (must happen before other setup)
            if let Some(ref ns_config) = def.namespace {
                if let Err(e) = crate::namespace::setup::apply_namespaces(ns_config) {
                    eprintln!("dynamod: namespace setup failed: {e}");
                    std::process::exit(126);
                }
            }

            // Change working directory if specified
            if let Some(ref workdir) = def.service.workdir {
                let _ = std::env::set_current_dir(Path::new(workdir));
            }

            // Drop privileges if user/group specified
            if let Some(ref user_section) = def.service.user {
                if let Err(e) = drop_privileges(user_section) {
                    eprintln!("dynamod: aborting service — privilege drop failed: {e}");
                    std::process::exit(126);
                }
            }

            // Exec the service binary
            let _ = unistd::execve(&program, &args, &env);

            // If exec fails, exit with 127
            std::process::exit(127);
        }
        ForkResult::Parent { child } => {
            tracing::info!(
                "spawned service '{}' (pid {})",
                def.service.name,
                child
            );

            // Close write end in parent, start log relay thread for read end
            if let Some((read_end, write_end)) = log_pipe {
                unsafe { libc::close(write_end); }
                let service_name = def.service.name.clone();
                std::thread::spawn(move || {
                    relay_logs(read_end, &service_name);
                });
            }

            // Handle ready-fd pipe: close write end, set read end non-blocking
            let ready_fd_read = if let Some((read_end, write_end)) = ready_pipe {
                unsafe {
                    libc::close(write_end);
                    // Set non-blocking so readiness polling doesn't block
                    let flags = libc::fcntl(read_end, libc::F_GETFL);
                    if flags >= 0 {
                        libc::fcntl(read_end, libc::F_SETFL, flags | libc::O_NONBLOCK);
                    }
                }
                Some(read_end)
            } else {
                None
            };

            // Open pidfd for the child
            let pidfd = open_pidfd(child);

            Ok(SpawnedProcess {
                pid: child,
                pidfd,
                ready_fd: ready_fd_read,
            })
        }
    }
}

/// Open a pidfd for a process (Linux 5.3+).
fn open_pidfd(pid: Pid) -> Option<RawFd> {
    let result = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
    if result >= 0 {
        Some(result as RawFd)
    } else {
        tracing::warn!("pidfd_open failed for pid {pid}");
        None
    }
}

/// Drop privileges to the specified user/group.
/// Order: resolve all IDs → setgroups → setgid → setuid (setuid must be last).
/// Returns an error if any privilege-drop syscall fails, so the caller can abort.
fn drop_privileges(user_section: &crate::config::service::UserSection) -> Result<(), String> {
    use nix::unistd::{setgid, setgroups, setuid, Gid, Group, Uid, User};

    // Resolve the target user (needed for default GID fallback)
    let resolved_user = match user_section.user.as_ref() {
        Some(name) => {
            if let Ok(uid) = name.parse::<u32>() {
                Some((Uid::from_raw(uid), None))
            } else {
                match User::from_name(name) {
                    Ok(Some(u)) => Some((u.uid, Some(u.gid))),
                    Ok(None) => return Err(format!("unknown user: {name}")),
                    Err(e) => return Err(format!("user lookup failed for '{name}': {e}")),
                }
            }
        }
        None => None,
    };

    // Resolve supplementary groups
    let mut supp_gids: Vec<Gid> = Vec::new();
    for group_name in &user_section.supplementary_groups {
        if let Ok(gid) = group_name.parse::<u32>() {
            supp_gids.push(Gid::from_raw(gid));
        } else {
            match Group::from_name(group_name) {
                Ok(Some(grp)) => supp_gids.push(grp.gid),
                Ok(None) => return Err(format!("unknown supplementary group: {group_name}")),
                Err(e) => {
                    return Err(format!("group lookup failed for '{group_name}': {e}"))
                }
            }
        }
    }

    // Set supplementary groups (must happen before setuid)
    if !supp_gids.is_empty() {
        setgroups(&supp_gids).map_err(|e| format!("setgroups failed: {e}"))?;
    }

    // Set primary group
    let target_gid = if let Some(ref group) = user_section.group {
        if let Ok(gid) = group.parse::<u32>() {
            Some(Gid::from_raw(gid))
        } else {
            match Group::from_name(group) {
                Ok(Some(grp)) => Some(grp.gid),
                Ok(None) => return Err(format!("unknown group: {group}")),
                Err(e) => return Err(format!("group lookup failed for '{group}': {e}")),
            }
        }
    } else {
        resolved_user.as_ref().and_then(|(_, gid)| *gid)
    };

    if let Some(gid) = target_gid {
        setgid(gid).map_err(|e| format!("setgid({gid}) failed: {e}"))?;
    }

    // Set user (must be last — drops ability to change back)
    if let Some((uid, _)) = resolved_user {
        setuid(uid).map_err(|e| format!("setuid({uid}) failed: {e}"))?;
    }

    Ok(())
}

/// Create a pipe, returning (read_fd, write_fd) as raw file descriptors.
fn create_pipe() -> Option<(RawFd, RawFd)> {
    let mut fds = [0 as RawFd; 2];
    let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if ret == 0 {
        Some((fds[0], fds[1]))
    } else {
        None
    }
}

/// Relay log lines from a pipe fd to the dynamod-logd socket.
/// Each line is sent as a datagram: "<service_name>\t<line>".
/// Runs until the pipe is closed (service exits).
fn relay_logs(read_fd: RawFd, service_name: &str) {
    let file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let reader = BufReader::new(file);
    let sock = UnixDatagram::unbound().ok();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }

        // Send to logd socket if available
        if let Some(ref sock) = sock {
            let msg = format!("{service_name}\t{line}");
            let _ = sock.send_to(msg.as_bytes(), LOG_SOCKET_PATH);
        }
    }
}

/// Send a signal to a process.
pub fn signal_process(pid: Pid, sig: Signal) -> Result<(), nix::Error> {
    nix::sys::signal::kill(pid, sig)
}
