use std::collections::HashMap;
use std::os::unix::io::RawFd;

use nix::sys::wait::{self, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

/// Information about an exited child process.
#[derive(Debug, Clone)]
pub struct ExitedChild {
    pub pid: Pid,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

/// Tracks running service processes by PID.
pub struct ProcessMonitor {
    /// Map from PID to service name.
    pid_to_service: HashMap<i32, String>,
    /// Map from service name to PID.
    service_to_pid: HashMap<String, i32>,
    /// Map from service name to pidfd.
    service_to_pidfd: HashMap<String, RawFd>,
}

impl ProcessMonitor {
    pub fn new() -> Self {
        Self {
            pid_to_service: HashMap::new(),
            service_to_pid: HashMap::new(),
            service_to_pidfd: HashMap::new(),
        }
    }

    /// Register a newly spawned service process.
    pub fn register(&mut self, service_name: &str, pid: Pid, pidfd: Option<RawFd>) {
        let raw_pid = pid.as_raw();
        self.pid_to_service
            .insert(raw_pid, service_name.to_string());
        self.service_to_pid
            .insert(service_name.to_string(), raw_pid);
        if let Some(fd) = pidfd {
            self.service_to_pidfd
                .insert(service_name.to_string(), fd);
        }
    }

    /// Unregister a service process (after it has exited).
    pub fn unregister(&mut self, service_name: &str) {
        if let Some(pid) = self.service_to_pid.remove(service_name) {
            self.pid_to_service.remove(&pid);
        }
        if let Some(fd) = self.service_to_pidfd.remove(service_name) {
            let _ = nix::unistd::close(fd);
        }
    }

    /// Look up the service name for a given PID.
    pub fn service_for_pid(&self, pid: i32) -> Option<&str> {
        self.pid_to_service.get(&pid).map(|s| s.as_str())
    }

    /// Get the PID for a given service.
    pub fn pid_for_service(&self, service_name: &str) -> Option<Pid> {
        self.service_to_pid
            .get(service_name)
            .map(|&p| Pid::from_raw(p))
    }

    /// Check if a service is currently running.
    pub fn is_running(&self, service_name: &str) -> bool {
        self.service_to_pid.contains_key(service_name)
    }

    /// Reap all available zombie children (non-blocking).
    /// Returns a list of exited children with their service names.
    pub fn reap_all(&mut self) -> Vec<(String, ExitedChild)> {
        let mut exited = Vec::new();

        loop {
            match wait::waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, code)) => {
                    let child = ExitedChild {
                        pid,
                        exit_code: Some(code),
                        signal: None,
                    };
                    if let Some(name) = self.pid_to_service.get(&pid.as_raw()).cloned() {
                        self.unregister(&name);
                        exited.push((name, child));
                    } else {
                        tracing::debug!("reaped unknown pid {pid} (exit code {code})");
                    }
                }
                Ok(WaitStatus::Signaled(pid, sig, _)) => {
                    let child = ExitedChild {
                        pid,
                        exit_code: None,
                        signal: Some(sig as i32),
                    };
                    if let Some(name) = self.pid_to_service.get(&pid.as_raw()).cloned() {
                        self.unregister(&name);
                        exited.push((name, child));
                    } else {
                        tracing::debug!("reaped unknown pid {pid} (signal {sig})");
                    }
                }
                Ok(WaitStatus::StillAlive) => break,
                Err(nix::errno::Errno::ECHILD) => break,
                _ => break,
            }
        }

        exited
    }

    /// Get all pidfds for epoll registration.
    pub fn pidfds(&self) -> impl Iterator<Item = (&str, RawFd)> {
        self.service_to_pidfd
            .iter()
            .map(|(name, &fd)| (name.as_str(), fd))
    }

    /// Get the number of tracked processes.
    pub fn count(&self) -> usize {
        self.service_to_pid.len()
    }
}
