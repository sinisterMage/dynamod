/// Inhibitor lock management for power actions.
///
/// Clients call `Inhibit(what, who, why, mode)` and receive a pipe fd.
/// The inhibitor remains active until the client closes the fd (detected
/// by the read end becoming readable with EOF).
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug)]
pub struct Inhibitor {
    /// What is being inhibited: colon-separated list of
    /// "shutdown", "sleep", "idle", "handle-power-key", etc.
    pub what: String,
    /// Human-readable name of who is inhibiting.
    pub who: String,
    /// Human-readable reason.
    pub why: String,
    /// "block" or "delay".
    pub mode: String,
    /// UID of the caller.
    pub uid: u32,
    /// PID of the caller.
    pub pid: u32,
    /// Cookie for identification.
    pub cookie: u64,
    /// Our end of the pipe (read end). When the client closes the write end,
    /// this becomes readable (EOF), and we remove the inhibitor.
    pub pipe_read: OwnedFd,
}

impl Inhibitor {
    /// Check whether the inhibitor's pipe has been closed by the client.
    /// Returns `true` if the inhibitor should be removed.
    pub fn is_released(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.pipe_read.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 0) };
        if ret > 0 {
            // Readable means EOF (client closed their end)
            pfd.revents & (libc::POLLIN | libc::POLLHUP) != 0
        } else {
            false
        }
    }
}

/// Create an inhibitor. Returns `(inhibitor, client_fd)`.
///
/// The client receives the write end of the pipe. When they close it,
/// we detect EOF on the read end and remove the inhibitor.
pub fn create_inhibitor(
    what: String,
    who: String,
    why: String,
    mode: String,
    uid: u32,
    pid: u32,
    cookie: u64,
) -> Result<(Inhibitor, OwnedFd), std::io::Error> {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    let inhibitor = Inhibitor {
        what,
        who,
        why,
        mode,
        uid,
        pid,
        cookie,
        pipe_read: read_end,
    };

    Ok((inhibitor, write_end))
}
