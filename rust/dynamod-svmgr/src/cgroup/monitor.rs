/// Cgroup v2 event monitoring.
///
/// Monitors memory.events for OOM kills and memory pressure events.
/// Uses inotify on cgroup control files to detect state changes.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Events that can be detected from cgroup monitoring.
#[derive(Debug, Clone)]
pub enum CgroupEvent {
    /// An OOM kill occurred in the service's cgroup.
    OomKill {
        service_name: String,
        count: u64,
    },
    /// Memory usage exceeds the high watermark.
    MemoryHigh {
        service_name: String,
        current_bytes: u64,
    },
}

/// Tracks cgroup event state for polling-based monitoring.
/// (A future version can use inotify for push-based monitoring.)
#[derive(Debug)]
pub struct CgroupMonitor {
    /// Map from service name to its cgroup path.
    watched: HashMap<String, PathBuf>,
    /// Last known oom_kill count per service.
    last_oom_count: HashMap<String, u64>,
}

impl CgroupMonitor {
    pub fn new() -> Self {
        Self {
            watched: HashMap::new(),
            last_oom_count: HashMap::new(),
        }
    }

    /// Start monitoring a service's cgroup.
    pub fn watch(&mut self, service_name: &str, cgroup_path: &Path) {
        self.watched
            .insert(service_name.to_string(), cgroup_path.to_path_buf());
        // Initialize the OOM count so we only report new kills
        let count = read_oom_kill_count(cgroup_path);
        self.last_oom_count
            .insert(service_name.to_string(), count);
    }

    /// Stop monitoring a service's cgroup.
    pub fn unwatch(&mut self, service_name: &str) {
        self.watched.remove(service_name);
        self.last_oom_count.remove(service_name);
    }

    /// Poll all watched cgroups for events.
    /// Returns a list of new events since the last poll.
    pub fn poll(&mut self) -> Vec<CgroupEvent> {
        let mut events = Vec::new();

        for (name, path) in &self.watched {
            // Check for new OOM kills
            let current_oom = read_oom_kill_count(path);
            let last_oom = self.last_oom_count.get(name).copied().unwrap_or(0);
            if current_oom > last_oom {
                events.push(CgroupEvent::OomKill {
                    service_name: name.clone(),
                    count: current_oom - last_oom,
                });
            }
            self.last_oom_count.insert(name.clone(), current_oom);

            // Check for memory pressure (current > high)
            if let (Some(current), Some(high)) = (
                read_memory_current(path),
                read_memory_high(path),
            ) {
                if current > high {
                    events.push(CgroupEvent::MemoryHigh {
                        service_name: name.clone(),
                        current_bytes: current,
                    });
                }
            }
        }

        events
    }

    /// Get the number of services being monitored.
    pub fn count(&self) -> usize {
        self.watched.len()
    }
}

/// Read the oom_kill count from memory.events.
fn read_oom_kill_count(cgroup_path: &Path) -> u64 {
    let events_path = cgroup_path.join("memory.events");
    let content = match fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("oom_kill ") {
            return rest.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Read current memory usage.
fn read_memory_current(cgroup_path: &Path) -> Option<u64> {
    let path = cgroup_path.join("memory.current");
    fs::read_to_string(&path).ok()?.trim().parse().ok()
}

/// Read memory.high limit.
fn read_memory_high(cgroup_path: &Path) -> Option<u64> {
    let path = cgroup_path.join("memory.high");
    let s = fs::read_to_string(&path).ok()?;
    let s = s.trim();
    if s == "max" {
        return None; // No limit set
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitor_empty() {
        let mut monitor = CgroupMonitor::new();
        assert_eq!(monitor.count(), 0);
        assert!(monitor.poll().is_empty());
    }

    #[test]
    fn test_watch_unwatch() {
        let mut monitor = CgroupMonitor::new();
        let path = Path::new("/tmp/nonexistent-cgroup");

        monitor.watch("test-svc", path);
        assert_eq!(monitor.count(), 1);

        monitor.unwatch("test-svc");
        assert_eq!(monitor.count(), 0);
    }

    #[test]
    fn test_poll_nonexistent_path_no_panic() {
        let mut monitor = CgroupMonitor::new();
        monitor.watch("test-svc", Path::new("/tmp/nonexistent-cgroup-xyz"));

        // Should not panic, just return no events
        let events = monitor.poll();
        assert!(events.is_empty());
    }
}
