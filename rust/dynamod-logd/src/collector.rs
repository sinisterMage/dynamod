/// Log stream collector.
///
/// Reads log lines from input streams (stdin, pipes, sockets)
/// and passes them to the storage backend.
use std::io::{self, BufRead};
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::storage::LogStorage;

/// Collect log lines from stdin until EOF.
pub fn collect_stdin(store: &mut LogStorage) -> io::Result<()> {
    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        store.append("stdin", &line);
    }

    Ok(())
}

/// Log socket path.
pub const LOG_SOCKET_PATH: &str = "/run/dynamod/log.sock";

/// Collect log datagrams from a Unix socket.
///
/// Each datagram has the format: `<service_name>\t<message>`
/// If no tab separator is found, "unknown" is used as the source.
pub fn collect_socket(store: Arc<Mutex<LogStorage>>) -> io::Result<()> {
    let sock_path = Path::new(LOG_SOCKET_PATH);

    // Remove stale socket file
    let _ = std::fs::remove_file(sock_path);

    let sock = UnixDatagram::bind(sock_path)?;

    // Set permissions so svmgr can write to it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o666));
    }

    tracing::info!("listening on {}", LOG_SOCKET_PATH);

    let mut buf = [0u8; 8192];
    loop {
        match sock.recv(&mut buf) {
            Ok(n) if n > 0 => {
                let msg = String::from_utf8_lossy(&buf[..n]);
                let (source, message) = if let Some((src, rest)) = msg.split_once('\t') {
                    (src, rest)
                } else {
                    ("unknown", msg.as_ref())
                };
                if let Ok(mut s) = store.lock() {
                    s.append(source, message);
                }
            }
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}
