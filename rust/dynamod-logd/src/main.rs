/// dynamod-logd: Log collection daemon for the dynamod init system.
///
/// Accepts log streams from services via a Unix datagram socket
/// at /run/dynamod/log.sock. Also reads from stdin for backward
/// compatibility.
mod collector;
mod storage;

use std::path::Path;
use std::sync::{Arc, Mutex};

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-logd starting");

    // Ensure log directory exists
    let log_dir = Path::new("/var/log/dynamod");
    if let Err(e) = std::fs::create_dir_all(log_dir) {
        tracing::warn!("failed to create {}: {e}", log_dir.display());
    }

    let store = Arc::new(Mutex::new(
        storage::LogStorage::new(log_dir, 10 * 1024 * 1024), // 10 MiB max
    ));

    // Start socket collector in a background thread
    let store_socket = Arc::clone(&store);
    std::thread::spawn(move || {
        if let Err(e) = collector::collect_socket(store_socket) {
            tracing::error!("socket collector stopped: {e}");
        }
    });

    // Collect from stdin on main thread (piped from svmgr or service).
    // stdin may be /dev/null (EOF immediately) when the service manager
    // redirects it; in that case we park the main thread so the socket
    // collector keeps running.
    tracing::info!("collecting logs from stdin and socket");
    {
        let mut store_guard = store.lock().unwrap();
        if let Err(e) = collector::collect_stdin(&mut store_guard) {
            tracing::error!("stdin collection stopped: {e}");
        }
    }

    tracing::info!("stdin closed, parking main thread (socket collector still active)");
    loop {
        std::thread::park();
    }
}
