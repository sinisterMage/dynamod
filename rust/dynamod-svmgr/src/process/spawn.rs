use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::path::Path;

use nix::sys::signal::Signal;
use nix::unistd::{self, ForkResult, Pid};

use crate::config::service::ServiceDef;

/// Information about a successfully spawned service process.
#[derive(Debug)]
pub struct SpawnedProcess {
    pub pid: Pid,
    pub pidfd: Option<RawFd>,
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
        .map(|a| CString::new(a.as_bytes()).unwrap_or_default())
        .collect();

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

    let env: Vec<CString> = env_map
        .iter()
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();

    // Fork
    let fork_result = unsafe { unistd::fork() }.map_err(SpawnError::Fork)?;

    match fork_result {
        ForkResult::Child => {
            // === Child process ===

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
                drop_privileges(user_section);
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

            // Open pidfd for the child
            let pidfd = open_pidfd(child);

            Ok(SpawnedProcess {
                pid: child,
                pidfd,
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
fn drop_privileges(user: &crate::config::service::UserSection) {
    use nix::unistd::{setgid, setuid, Gid, Uid};

    // Set supplementary groups first (must be done before setuid)
    if !user.supplementary_groups.is_empty() {
        // Would need to resolve group names to GIDs
        // For now, just log a warning
        tracing::warn!("supplementary groups not yet implemented");
    }

    // Set group
    if let Some(ref group) = user.group {
        if let Ok(gid) = group.parse::<u32>() {
            let _ = setgid(Gid::from_raw(gid));
        }
        // TODO: resolve group name to GID via getgrnam
    }

    // Set user (must be last)
    if let Some(ref user_name) = user.user {
        if let Ok(uid) = user_name.parse::<u32>() {
            let _ = setuid(Uid::from_raw(uid));
        }
        // TODO: resolve username to UID via getpwnam
    }
}

/// Send a signal to a process.
pub fn signal_process(pid: Pid, sig: Signal) -> Result<(), nix::Error> {
    nix::sys::signal::kill(pid, sig)
}
