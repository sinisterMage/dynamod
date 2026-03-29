/// Service readiness detection.
///
/// Supports multiple readiness mechanisms:
/// - **none**: Ready immediately after exec
/// - **notify**: sd_notify compatible (READY=1 on a Unix datagram socket)
/// - **tcp-port**: Poll a TCP port until it accepts connections
/// - **exec**: Run a health-check command; ready when it exits 0
/// - **fd**: Service writes a byte to a passed file descriptor
use std::net::TcpStream;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::service::{ReadinessSection, ReadinessType};

/// Result of a readiness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessResult {
    /// Service is ready.
    Ready,
    /// Still waiting for readiness.
    NotReady,
    /// Readiness check timed out.
    TimedOut,
    /// Readiness check failed (unrecoverable).
    Failed(String),
}

/// Tracks readiness state for a single service.
#[derive(Debug)]
pub struct ReadinessTracker {
    readiness_type: ReadinessType,
    port: Option<u16>,
    check_exec: Option<Vec<String>>,
    timeout: Duration,
    started_at: Instant,
    notify_socket: Option<PathBuf>,
    /// Bound notify socket (created once, polled on each check)
    bound_notify: Option<UnixDatagram>,
    /// Read end of the fd-based readiness pipe (parent polls this).
    ready_fd: Option<RawFd>,
}

impl ReadinessTracker {
    /// Create a readiness tracker from the service's readiness config.
    pub fn new(config: &ReadinessSection, service_name: &str) -> Self {
        let timeout = crate::config::service::parse_duration_secs(&config.timeout)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));

        let notify_socket = if config.readiness_type == ReadinessType::Notify {
            Some(PathBuf::from(format!(
                "/run/dynamod/notify-{}.sock",
                service_name
            )))
        } else {
            None
        };

        // Bind the notify socket once at construction
        let bound_notify = notify_socket.as_ref().and_then(|path| {
            let _ = std::fs::remove_file(path);
            match UnixDatagram::bind(path) {
                Ok(sock) => {
                    sock.set_nonblocking(true).ok();
                    Some(sock)
                }
                Err(e) => {
                    tracing::warn!("failed to bind notify socket {}: {e}", path.display());
                    None
                }
            }
        });

        Self {
            readiness_type: config.readiness_type.clone(),
            port: config.port,
            check_exec: config.check_exec.clone(),
            timeout,
            started_at: Instant::now(),
            notify_socket,
            bound_notify,
            ready_fd: None,
        }
    }

    /// Get the NOTIFY_SOCKET path (for sd_notify compatible services).
    pub fn notify_socket_path(&self) -> Option<&PathBuf> {
        self.notify_socket.as_ref()
    }

    /// Set the read end of the fd-based readiness pipe.
    pub fn set_ready_fd(&mut self, fd: RawFd) {
        self.ready_fd = Some(fd);
    }

    /// Check if this service is immediately ready (type = none).
    pub fn is_immediate(&self) -> bool {
        self.readiness_type == ReadinessType::None
    }

    /// Perform a readiness check. Non-blocking.
    pub fn check(&self) -> ReadinessResult {
        if self.started_at.elapsed() > self.timeout {
            return ReadinessResult::TimedOut;
        }

        match self.readiness_type {
            ReadinessType::None => ReadinessResult::Ready,
            ReadinessType::TcpPort => self.check_tcp_port(),
            ReadinessType::Exec => self.check_exec(),
            ReadinessType::Notify => self.check_notify(),
            ReadinessType::Fd => self.check_fd(),
        }
    }

    /// Check if a TCP port is accepting connections.
    fn check_tcp_port(&self) -> ReadinessResult {
        let port = match self.port {
            Some(p) => p,
            None => return ReadinessResult::Failed("no port configured".into()),
        };

        let addr = format!("127.0.0.1:{port}");
        match TcpStream::connect_timeout(
            &addr.parse().unwrap(),
            Duration::from_millis(200),
        ) {
            Ok(_) => ReadinessResult::Ready,
            Err(_) => ReadinessResult::NotReady,
        }
    }

    /// Run a health-check command and check if it exits 0.
    fn check_exec(&self) -> ReadinessResult {
        let cmd = match &self.check_exec {
            Some(c) if !c.is_empty() => c,
            _ => return ReadinessResult::Failed("no check-exec configured".into()),
        };

        match std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() => ReadinessResult::Ready,
            Ok(_) => ReadinessResult::NotReady,
            Err(e) => ReadinessResult::Failed(format!("check-exec error: {e}")),
        }
    }

    /// Check for sd_notify readiness (READY=1 on the notify socket).
    fn check_notify(&self) -> ReadinessResult {
        let sock = match &self.bound_notify {
            Some(s) => s,
            None => return ReadinessResult::Failed("no notify socket".into()),
        };

        let mut buf = [0u8; 256];
        match sock.recv(&mut buf) {
            Ok(n) => {
                let msg = std::str::from_utf8(&buf[..n]).unwrap_or("");
                if msg.contains("READY=1") {
                    // Clean up socket file
                    if let Some(ref path) = self.notify_socket {
                        let _ = std::fs::remove_file(path);
                    }
                    ReadinessResult::Ready
                } else {
                    ReadinessResult::NotReady
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ReadinessResult::NotReady
            }
            Err(_) => ReadinessResult::NotReady,
        }
    }

    /// Check for fd-based readiness (service writes a byte to the pipe).
    fn check_fd(&self) -> ReadinessResult {
        let fd = match self.ready_fd {
            Some(fd) => fd,
            None => return ReadinessResult::Failed("no ready fd configured".into()),
        };

        // Non-blocking read: check if any byte has arrived
        let mut buf = [0u8; 1];
        let result = unsafe {
            libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1)
        };

        if result > 0 {
            // Close the read end now that we've received the signal
            unsafe { libc::close(fd); }
            ReadinessResult::Ready
        } else if result == 0 {
            // Pipe closed without writing — service exited without signaling
            unsafe { libc::close(fd); }
            ReadinessResult::Failed("ready fd closed without signal".into())
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                ReadinessResult::NotReady
            } else {
                ReadinessResult::Failed(format!("ready fd read error: {err}"))
            }
        }
    }

    /// Reset the timer (e.g. after a restart).
    pub fn reset(&mut self) {
        self.started_at = Instant::now();
    }
}

/// Poll readiness for a service with retries.
/// This is a blocking helper for simple cases; the real event loop
/// will use non-blocking checks.
pub fn wait_for_ready(
    tracker: &ReadinessTracker,
    poll_interval: Duration,
) -> ReadinessResult {
    if tracker.is_immediate() {
        return ReadinessResult::Ready;
    }

    loop {
        match tracker.check() {
            ReadinessResult::NotReady => {
                std::thread::sleep(poll_interval);
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_readiness_immediate() {
        let config = ReadinessSection {
            readiness_type: ReadinessType::None,
            port: None,
            check_exec: None,
            timeout: "30s".into(),
        };
        let tracker = ReadinessTracker::new(&config, "test");
        assert!(tracker.is_immediate());
        assert_eq!(tracker.check(), ReadinessResult::Ready);
    }

    #[test]
    fn test_tcp_port_not_ready() {
        let config = ReadinessSection {
            readiness_type: ReadinessType::TcpPort,
            port: Some(19999), // unlikely to be listening
            check_exec: None,
            timeout: "5s".into(),
        };
        let tracker = ReadinessTracker::new(&config, "test");
        assert!(!tracker.is_immediate());
        assert_eq!(tracker.check(), ReadinessResult::NotReady);
    }

    #[test]
    fn test_exec_readiness_true() {
        let config = ReadinessSection {
            readiness_type: ReadinessType::Exec,
            port: None,
            check_exec: Some(vec!["true".into()]),
            timeout: "5s".into(),
        };
        let tracker = ReadinessTracker::new(&config, "test");
        assert_eq!(tracker.check(), ReadinessResult::Ready);
    }

    #[test]
    fn test_exec_readiness_false() {
        let config = ReadinessSection {
            readiness_type: ReadinessType::Exec,
            port: None,
            check_exec: Some(vec!["false".into()]),
            timeout: "5s".into(),
        };
        let tracker = ReadinessTracker::new(&config, "test");
        assert_eq!(tracker.check(), ReadinessResult::NotReady);
    }
}
