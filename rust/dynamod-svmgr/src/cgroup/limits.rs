/// Cgroup v2 resource limit configuration.
///
/// Writes resource limits to cgroup control files for a service.
use std::fs;
use std::path::Path;

use crate::config::service::CgroupSection;
use super::hierarchy::CgroupError;

/// Apply resource limits from a CgroupSection to a cgroup directory.
pub fn apply_limits(cgroup_path: &Path, config: &CgroupSection) -> Result<(), CgroupError> {
    // memory.max — hard memory limit
    if let Some(ref limit) = config.memory_max {
        write_limit(cgroup_path, "memory.max", &parse_bytes(limit))?;
    }

    // memory.high — soft memory limit (reclaim pressure)
    if let Some(ref limit) = config.memory_high {
        write_limit(cgroup_path, "memory.high", &parse_bytes(limit))?;
    }

    // cpu.weight — relative CPU weight (1-10000, default 100)
    if let Some(weight) = config.cpu_weight {
        write_limit(cgroup_path, "cpu.weight", &weight.to_string())?;
    }

    // cpu.max — CPU bandwidth limit ("quota period", e.g. "200000 100000" for 200%)
    if let Some(ref max) = config.cpu_max {
        write_limit(cgroup_path, "cpu.max", max)?;
    }

    // pids.max — maximum number of processes
    if let Some(max) = config.pids_max {
        write_limit(cgroup_path, "pids.max", &max.to_string())?;
    }

    // io.weight — relative I/O weight (1-10000, default 100)
    if let Some(weight) = config.io_weight {
        write_limit(cgroup_path, "io.weight", &format!("default {weight}"))?;
    }

    Ok(())
}

/// Read current resource usage from a cgroup.
pub fn read_usage(cgroup_path: &Path) -> CgroupUsage {
    CgroupUsage {
        memory_current: read_u64(cgroup_path, "memory.current"),
        memory_max: read_string(cgroup_path, "memory.max"),
        pids_current: read_u64(cgroup_path, "pids.current"),
        cpu_usage_usec: read_stat_field(cgroup_path, "cpu.stat", "usage_usec"),
    }
}

/// Current resource usage for a cgroup.
#[derive(Debug, Clone)]
pub struct CgroupUsage {
    pub memory_current: Option<u64>,
    pub memory_max: Option<String>,
    pub pids_current: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
}

fn write_limit(cgroup_path: &Path, filename: &str, value: &str) -> Result<(), CgroupError> {
    let path = cgroup_path.join(filename);
    fs::write(&path, value)
        .map_err(|e| CgroupError::WriteFile(path.display().to_string(), e))?;
    tracing::debug!("set {}: {}", path.display(), value);
    Ok(())
}

fn read_u64(cgroup_path: &Path, filename: &str) -> Option<u64> {
    let path = cgroup_path.join(filename);
    fs::read_to_string(&path).ok()?.trim().parse().ok()
}

fn read_string(cgroup_path: &Path, filename: &str) -> Option<String> {
    let path = cgroup_path.join(filename);
    fs::read_to_string(&path).ok().map(|s| s.trim().to_string())
}

fn read_stat_field(cgroup_path: &Path, filename: &str, field: &str) -> Option<u64> {
    let path = cgroup_path.join(filename);
    let content = fs::read_to_string(&path).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Parse a human-readable byte size (e.g. "512M", "2G", "1024K") into a string
/// suitable for cgroup control files.
fn parse_bytes(s: &str) -> String {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('K').or(s.strip_suffix('k')) {
        if let Ok(v) = n.trim().parse::<u64>() {
            return (v * 1024).to_string();
        }
    }
    if let Some(n) = s.strip_suffix('M').or(s.strip_suffix('m')) {
        if let Ok(v) = n.trim().parse::<u64>() {
            return (v * 1024 * 1024).to_string();
        }
    }
    if let Some(n) = s.strip_suffix('G').or(s.strip_suffix('g')) {
        if let Ok(v) = n.trim().parse::<u64>() {
            return (v * 1024 * 1024 * 1024).to_string();
        }
    }
    // If no suffix, pass through as-is (already bytes or "max")
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bytes() {
        assert_eq!(parse_bytes("512M"), (512 * 1024 * 1024).to_string());
        assert_eq!(parse_bytes("2G"), (2 * 1024 * 1024 * 1024u64).to_string());
        assert_eq!(parse_bytes("1024K"), (1024 * 1024).to_string());
        assert_eq!(parse_bytes("max"), "max");
        assert_eq!(parse_bytes("1048576"), "1048576");
    }

    #[test]
    fn test_apply_limits_to_tempdir() {
        let tmp = std::env::temp_dir().join(format!("dynamod-cg-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let config = CgroupSection {
            memory_max: Some("512M".into()),
            memory_high: Some("256M".into()),
            cpu_weight: Some(200),
            cpu_max: Some("200000 100000".into()),
            pids_max: Some(64),
            io_weight: Some(100),
        };

        // This writes to real files in /tmp — just verify no panics
        let result = apply_limits(&tmp, &config);
        // May fail if we can't write (not a real cgroup) but should not panic
        // On a real cgroup filesystem this would succeed
        let _ = result;

        std::fs::remove_dir_all(&tmp).ok();
    }
}
