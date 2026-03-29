/// libsystemd.so shim for dynamoD.
///
/// Provides the most commonly used functions from libsystemd so that
/// applications linked against -lsystemd work transparently on dynamoD.
///
/// Clean-room implementation based on public sd-daemon(3) and sd-journal(3)
/// man pages. No systemd source code was used.
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

// ============================================================
// sd-daemon: sd_notify, sd_listen_fds, sd_booted
// ============================================================

/// Send a notification to the service manager.
///
/// If `unset_environment` is non-zero, unset $NOTIFY_SOCKET after sending.
/// `state` is a newline-separated list of "VARIABLE=value" pairs.
/// Returns >0 on success, 0 if $NOTIFY_SOCKET is not set, <0 on error.
#[unsafe(no_mangle)]
pub extern "C" fn sd_notify(unset_environment: c_int, state: *const c_char) -> c_int {
    let state_str = match unsafe { CStr::from_ptr(state) }.to_str() {
        Ok(s) => s,
        Err(_) => return -libc::EINVAL,
    };

    let socket_path = match std::env::var("NOTIFY_SOCKET") {
        Ok(p) => p,
        Err(_) => return 0,
    };

    if socket_path.is_empty() {
        return 0;
    }

    let sock = match std::os::unix::net::UnixDatagram::unbound() {
        Ok(s) => s,
        Err(_) => return -libc::ECONNREFUSED,
    };

    let target = if socket_path.starts_with('@') {
        // Abstract socket: replace @ with null byte
        format!("\0{}", &socket_path[1..])
    } else {
        socket_path.clone()
    };

    match sock.send_to(state_str.as_bytes(), &target) {
        Ok(_) => {
            if unset_environment != 0 {
                // SAFETY: we are not reading env vars from another thread
                unsafe { std::env::remove_var("NOTIFY_SOCKET") };
            }
            1
        }
        Err(_) => -libc::ECONNREFUSED,
    }
}

/// sd_notify with PID (for multi-process services).
#[unsafe(no_mangle)]
pub extern "C" fn sd_pid_notify(
    pid: libc::pid_t,
    unset_environment: c_int,
    state: *const c_char,
) -> c_int {
    let _ = pid; // We don't need PID for our implementation
    sd_notify(unset_environment, state)
}

/// Return the number of file descriptors passed by the service manager.
///
/// The fds start at fd 3 (SD_LISTEN_FDS_START).
/// If `unset_environment` is non-zero, unset $LISTEN_FDS and $LISTEN_PID.
/// Returns the count, or 0 if none.
#[unsafe(no_mangle)]
pub extern "C" fn sd_listen_fds(unset_environment: c_int) -> c_int {
    let pid_str = match std::env::var("LISTEN_PID") {
        Ok(s) => s,
        Err(_) => return 0,
    };

    let expected_pid: u32 = match pid_str.parse() {
        Ok(p) => p,
        Err(_) => return 0,
    };

    // Only return fds if they're meant for us
    let my_pid = unsafe { libc::getpid() } as u32;
    if expected_pid != my_pid {
        return 0;
    }

    let count_str = match std::env::var("LISTEN_FDS") {
        Ok(s) => s,
        Err(_) => return 0,
    };

    let count: c_int = match count_str.parse() {
        Ok(c) => c,
        Err(_) => return 0,
    };

    if unset_environment != 0 {
        // SAFETY: we are not reading env vars from another thread
        unsafe {
            std::env::remove_var("LISTEN_FDS");
            std::env::remove_var("LISTEN_PID");
            std::env::remove_var("LISTEN_FDNAMES");
        }
    }

    // Unset CLOEXEC on the inherited fds
    for i in 0..count {
        let fd = 3 + i;
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }

    count
}

/// Check whether the system was booted with systemd (or in our case, dynamoD).
///
/// Returns >0 if running under dynamoD, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn sd_booted() -> c_int {
    // Check for dynamod runtime directory
    if std::fs::metadata("/run/dynamod").is_ok() {
        return 1;
    }
    // Also check for the systemd compatibility marker
    if std::fs::metadata("/run/systemd/system").is_ok() {
        return 1;
    }
    0
}

/// Constant for the first passed fd.
#[unsafe(no_mangle)]
pub static SD_LISTEN_FDS_START: c_int = 3;

// ============================================================
// sd-journal: sd_journal_print, sd_journal_send
// ============================================================

/// Print a message to the journal (maps to syslog/stderr).
///
/// Priority levels match syslog: 0=emerg, 3=err, 4=warn, 6=info, 7=debug.
///
/// Note: This simplified version does not process printf format specifiers.
/// The format string is treated as the message itself. This is sufficient
/// for most callers since sd_journal_print is typically called with a
/// pre-formatted string.
#[unsafe(no_mangle)]
pub extern "C" fn sd_journal_print(priority: c_int, format: *const c_char) -> c_int {
    let msg = match unsafe { CStr::from_ptr(format) }.to_str() {
        Ok(s) => s,
        Err(_) => return -libc::EINVAL,
    };

    // Write to stderr with priority prefix
    let prefix = match priority {
        0 => "<0>", // emerg
        1 => "<1>", // alert
        2 => "<2>", // crit
        3 => "<3>", // err
        4 => "<4>", // warning
        5 => "<5>", // notice
        6 => "<6>", // info
        7 => "<7>", // debug
        _ => "<6>",
    };

    // Try /dev/log first (syslog socket), fall back to stderr
    if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
        let syslog_msg = format!("{prefix}{msg}");
        if sock.send_to(syslog_msg.as_bytes(), "/dev/log").is_ok() {
            return 0;
        }
    }

    // Fallback: write to stderr
    eprintln!("{prefix}{msg}");
    0
}

/// Send a structured message to the journal.
///
/// In the real libsystemd this takes a NULL-terminated vararg list of
/// "KEY=value" strings. Since stable Rust does not support C variadics,
/// this simplified version accepts just the first argument (usually
/// "MESSAGE=...") and logs it. Most callers pass the message as the
/// first argument anyway.
#[unsafe(no_mangle)]
pub extern "C" fn sd_journal_send(format: *const c_char) -> c_int {
    if format.is_null() {
        return -libc::EINVAL;
    }

    let first = match unsafe { CStr::from_ptr(format) }.to_str() {
        Ok(s) => s,
        Err(_) => return -libc::EINVAL,
    };

    // Extract MESSAGE= and PRIORITY= from the first arg
    let message = first
        .strip_prefix("MESSAGE=")
        .unwrap_or(first);

    let c_msg = match CString::new(message) {
        Ok(c) => c,
        Err(_) => return -libc::EINVAL,
    };
    sd_journal_print(6, c_msg.as_ptr())
}

/// Open a stream fd for logging to the journal.
///
/// Returns a file descriptor that can be used as stdout/stderr,
/// where each line written becomes a journal entry.
#[unsafe(no_mangle)]
pub extern "C" fn sd_journal_stream_fd(
    _identifier: *const c_char,
    _priority: c_int,
    _level_prefix: c_int,
) -> c_int {
    // Return stderr fd as a fallback
    2
}

// ============================================================
// sd-login: session queries
// ============================================================

/// Get the session ID for a given PID.
///
/// Allocates a string via malloc that the caller must free.
#[unsafe(no_mangle)]
pub extern "C" fn sd_pid_get_session(
    pid: libc::pid_t,
    ret_session: *mut *mut c_char,
) -> c_int {
    if ret_session.is_null() {
        return -libc::EINVAL;
    }

    // Read from /proc/<pid>/sessionid or try cgroup-based lookup
    let session_id = if pid == 0 {
        // Use our own PID
        read_session_id(unsafe { libc::getpid() })
    } else {
        read_session_id(pid)
    };

    match session_id {
        Some(id) => {
            let c_str = match CString::new(id) {
                Ok(c) => c,
                Err(_) => return -libc::ENOMEM,
            };
            let ptr = unsafe { libc::strdup(c_str.as_ptr()) };
            if ptr.is_null() {
                return -libc::ENOMEM;
            }
            unsafe {
                *ret_session = ptr;
            }
            0
        }
        None => -libc::ENODATA,
    }
}

/// Get the session type (x11, wayland, tty, etc).
#[unsafe(no_mangle)]
pub extern "C" fn sd_session_get_type(
    session: *const c_char,
    ret_type: *mut *mut c_char,
) -> c_int {
    if session.is_null() || ret_type.is_null() {
        return -libc::EINVAL;
    }

    // Default to "tty" - in a full implementation this would query logind
    let type_str = CString::new("tty").unwrap();
    let ptr = unsafe { libc::strdup(type_str.as_ptr()) };
    if ptr.is_null() {
        return -libc::ENOMEM;
    }
    unsafe {
        *ret_type = ptr;
    }
    0
}

/// Get the seat for a session.
#[unsafe(no_mangle)]
pub extern "C" fn sd_session_get_seat(
    session: *const c_char,
    ret_seat: *mut *mut c_char,
) -> c_int {
    if session.is_null() || ret_seat.is_null() {
        return -libc::EINVAL;
    }

    let seat_str = CString::new("seat0").unwrap();
    let ptr = unsafe { libc::strdup(seat_str.as_ptr()) };
    if ptr.is_null() {
        return -libc::ENOMEM;
    }
    unsafe {
        *ret_seat = ptr;
    }
    0
}

/// Check if a session is active.
#[unsafe(no_mangle)]
pub extern "C" fn sd_session_is_active(session: *const c_char) -> c_int {
    if session.is_null() {
        return -libc::EINVAL;
    }
    // Default: yes
    1
}

/// Get the active session and uid on a seat.
#[unsafe(no_mangle)]
pub extern "C" fn sd_seat_get_active(
    seat: *const c_char,
    ret_session: *mut *mut c_char,
    ret_uid: *mut libc::uid_t,
) -> c_int {
    let _ = seat;

    if !ret_session.is_null() {
        let s = CString::new("1").unwrap();
        let ptr = unsafe { libc::strdup(s.as_ptr()) };
        if !ptr.is_null() {
            unsafe { *ret_session = ptr; }
        }
    }
    if !ret_uid.is_null() {
        unsafe { *ret_uid = libc::getuid(); }
    }
    0
}

// --- internal helpers ---

fn read_session_id(pid: libc::pid_t) -> Option<String> {
    // Try reading from /proc/PID/sessionid
    let path = format!("/proc/{pid}/sessionid");
    let id = std::fs::read_to_string(path).ok()?;
    let id = id.trim();
    if id == "4294967295" {
        // Unset sentinel value
        None
    } else {
        Some(id.to_string())
    }
}
