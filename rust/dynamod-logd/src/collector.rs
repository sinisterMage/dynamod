/// Log stream collector.
///
/// Reads log lines from input streams (stdin, pipes, sockets)
/// and passes them to the storage backend.
use std::io::{self, BufRead};

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
