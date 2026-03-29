/// Log storage backend.
///
/// Stores log entries in a file with automatic rotation when
/// the file exceeds the configured size limit.
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A single log entry.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: SystemTime,
    pub source: String,
    pub message: String,
}

/// Ring-buffer log storage with file persistence.
pub struct LogStorage {
    log_dir: PathBuf,
    current_file: Option<File>,
    current_size: u64,
    max_size: u64,
    /// In-memory ring buffer for recent logs (for queries).
    recent: VecDeque<LogEntry>,
    max_recent: usize,
}

impl LogStorage {
    pub fn new(log_dir: &Path, max_size: u64) -> Self {
        let current_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("current.log"))
            .ok();

        let current_size = current_file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);

        Self {
            log_dir: log_dir.to_path_buf(),
            current_file,
            current_size,
            max_size,
            recent: VecDeque::with_capacity(1000),
            max_recent: 1000,
        }
    }

    /// Append a log line.
    pub fn append(&mut self, source: &str, message: &str) {
        let now = SystemTime::now();
        let timestamp = humantime(now);

        let entry = LogEntry {
            timestamp: now,
            source: source.to_string(),
            message: message.to_string(),
        };

        // Write to file
        if let Some(ref mut file) = self.current_file {
            let line = format!("{timestamp} [{source}] {message}\n");
            if let Ok(n) = file.write(line.as_bytes()) {
                self.current_size += n as u64;
            }

            // Rotate if needed
            if self.current_size >= self.max_size {
                self.rotate();
            }
        }

        // Add to in-memory ring buffer
        if self.recent.len() >= self.max_recent {
            self.recent.pop_front();
        }
        self.recent.push_back(entry);
    }

    /// Get recent log entries.
    pub fn recent(&self, count: usize) -> Vec<&LogEntry> {
        self.recent.iter().rev().take(count).collect()
    }

    /// Rotate the current log file.
    fn rotate(&mut self) {
        let current_path = self.log_dir.join("current.log");
        let rotated_path = self.log_dir.join(format!(
            "log-{}.log",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ));

        // Close current file
        self.current_file = None;

        // Rename
        let _ = fs::rename(&current_path, &rotated_path);

        // Open new file
        self.current_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current_path)
            .ok();
        self.current_size = 0;

        tracing::info!("rotated log to {}", rotated_path.display());

        // Clean up old rotated files (keep last 5)
        self.cleanup_old_logs();
    }

    fn cleanup_old_logs(&self) {
        let mut log_files: Vec<PathBuf> = fs::read_dir(&self.log_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("log-") && n.ends_with(".log"))
            })
            .collect();

        log_files.sort();

        // Keep only the last 5
        while log_files.len() > 5 {
            if let Some(old) = log_files.first() {
                let _ = fs::remove_file(old);
                log_files.remove(0);
            }
        }
    }
}

fn humantime(t: SystemTime) -> String {
    let d = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    // Simple UTC timestamp
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{hours:02}:{mins:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_and_recent() {
        let tmp = std::env::temp_dir().join(format!("dynamod-log-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let mut store = LogStorage::new(&tmp, 1024 * 1024);
        store.append("test", "hello world");
        store.append("test", "second line");

        let recent = store.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "second line"); // Most recent first
        assert_eq!(recent[1].message, "hello world");

        // Check file was written
        let content = fs::read_to_string(tmp.join("current.log")).unwrap();
        assert!(content.contains("hello world"));
        assert!(content.contains("second line"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_rotation() {
        let tmp = std::env::temp_dir().join(format!("dynamod-logrot-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        // Set a very small max size to trigger rotation
        let mut store = LogStorage::new(&tmp, 50);
        for i in 0..10 {
            store.append("test", &format!("line number {i} with some padding"));
        }

        // Should have rotated at least once
        let entries: Vec<_> = fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.len() > 1); // current.log + at least one rotated file

        fs::remove_dir_all(&tmp).ok();
    }
}
