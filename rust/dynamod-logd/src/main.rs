/// dynamod-logd: Log collection daemon for the dynamod init system.
///
/// Accepts log streams from services via inherited pipe fds or
/// a Unix socket. Stores logs in a ring buffer and provides
/// a query interface.
///
/// Phase 6: Basic implementation that reads from stdin (pipe from svmgr)
/// and writes to a log file.
mod collector;
mod storage;

use std::path::Path;

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

    let mut store = storage::LogStorage::new(log_dir, 10 * 1024 * 1024); // 10 MiB max

    // Collect from stdin (piped from svmgr)
    tracing::info!("collecting logs from stdin");
    if let Err(e) = collector::collect_stdin(&mut store) {
        tracing::error!("log collection stopped: {e}");
    }

    tracing::info!("dynamod-logd exiting");
}
