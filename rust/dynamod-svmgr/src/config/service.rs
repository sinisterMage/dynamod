use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// A parsed service definition from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceDef {
    pub service: ServiceSection,
    #[serde(default)]
    pub restart: RestartSection,
    #[serde(default)]
    pub readiness: ReadinessSection,
    #[serde(default)]
    pub dependencies: DependencySection,
    #[serde(default)]
    pub cgroup: Option<CgroupSection>,
    #[serde(default)]
    pub namespace: Option<NamespaceSection>,
    #[serde(default)]
    pub shutdown: ShutdownSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceSection {
    pub name: String,
    #[serde(default = "default_supervisor")]
    pub supervisor: String,
    pub exec: Vec<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(rename = "type", default = "default_service_type")]
    pub service_type: ServiceType,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(rename = "environment-file")]
    #[serde(default)]
    pub environment_file: Option<String>,
    #[serde(default)]
    pub user: Option<UserSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserSection {
    pub user: Option<String>,
    pub group: Option<String>,
    #[serde(rename = "supplementary-groups", default)]
    pub supplementary_groups: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Simple,
    Oneshot,
    Forking,
    Notify,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RestartSection {
    #[serde(default = "default_restart_policy")]
    pub policy: RestartPolicy,
    #[serde(default = "default_delay")]
    pub delay: String,
    #[serde(rename = "max-restarts", default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(rename = "max-restart-window", default = "default_max_restart_window")]
    pub max_restart_window: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RestartPolicy {
    Permanent,
    Transient,
    Temporary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadinessSection {
    #[serde(rename = "type", default = "default_readiness_type")]
    pub readiness_type: ReadinessType,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(rename = "check-exec")]
    #[serde(default)]
    pub check_exec: Option<Vec<String>>,
    #[serde(default = "default_readiness_timeout")]
    pub timeout: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReadinessType {
    None,
    Notify,
    TcpPort,
    Exec,
    Fd,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DependencySection {
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub wants: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CgroupSection {
    #[serde(rename = "memory-max")]
    pub memory_max: Option<String>,
    #[serde(rename = "memory-high")]
    pub memory_high: Option<String>,
    #[serde(rename = "cpu-weight")]
    pub cpu_weight: Option<u32>,
    #[serde(rename = "cpu-max")]
    pub cpu_max: Option<String>,
    #[serde(rename = "pids-max")]
    pub pids_max: Option<u32>,
    #[serde(rename = "io-weight")]
    pub io_weight: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NamespaceSection {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(rename = "bind-mounts", default)]
    pub bind_mounts: Vec<BindMount>,
    #[serde(rename = "private-tmp", default)]
    pub private_tmp: bool,
    #[serde(rename = "protect-system")]
    pub protect_system: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BindMount {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShutdownSection {
    #[serde(rename = "stop-signal", default = "default_stop_signal")]
    pub stop_signal: String,
    #[serde(rename = "stop-timeout", default = "default_stop_timeout")]
    pub stop_timeout: String,
    #[serde(rename = "stop-exec")]
    #[serde(default)]
    pub stop_exec: Option<Vec<String>>,
}

// Defaults
fn default_supervisor() -> String { "root".into() }
fn default_service_type() -> ServiceType { ServiceType::Simple }
fn default_restart_policy() -> RestartPolicy { RestartPolicy::Permanent }
fn default_delay() -> String { "1s".into() }
fn default_max_restarts() -> u32 { 5 }
fn default_max_restart_window() -> String { "60s".into() }
fn default_readiness_type() -> ReadinessType { ReadinessType::None }
fn default_readiness_timeout() -> String { "30s".into() }
fn default_stop_signal() -> String { "SIGTERM".into() }
fn default_stop_timeout() -> String { "10s".into() }

impl Default for RestartSection {
    fn default() -> Self {
        Self {
            policy: default_restart_policy(),
            delay: default_delay(),
            max_restarts: default_max_restarts(),
            max_restart_window: default_max_restart_window(),
        }
    }
}

impl Default for ReadinessSection {
    fn default() -> Self {
        Self {
            readiness_type: default_readiness_type(),
            port: None,
            check_exec: None,
            timeout: default_readiness_timeout(),
        }
    }
}

impl Default for DependencySection {
    fn default() -> Self {
        Self {
            requires: vec![],
            wants: vec![],
            after: vec![],
            before: vec![],
        }
    }
}

impl Default for ShutdownSection {
    fn default() -> Self {
        Self {
            stop_signal: default_stop_signal(),
            stop_timeout: default_stop_timeout(),
            stop_exec: None,
        }
    }
}

/// Load a service definition from a TOML file.
pub fn load_service(path: &Path) -> Result<ServiceDef, ServiceLoadError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ServiceLoadError::Io(path.display().to_string(), e))?;
    let def: ServiceDef = toml::from_str(&content)
        .map_err(|e| ServiceLoadError::Parse(path.display().to_string(), e))?;
    Ok(def)
}

/// Load all service definitions from a directory.
pub fn load_services_dir(dir: &Path) -> Result<Vec<ServiceDef>, ServiceLoadError> {
    let mut services = Vec::new();

    let entries = std::fs::read_dir(dir)
        .map_err(|e| ServiceLoadError::Io(dir.display().to_string(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| ServiceLoadError::Io(dir.display().to_string(), e))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            match load_service(&path) {
                Ok(def) => services.push(def),
                Err(e) => {
                    tracing::warn!("skipping {}: {e}", path.display());
                }
            }
        }
    }

    Ok(services)
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceLoadError {
    #[error("I/O error reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parse error in {0}: {1}")]
    Parse(String, toml::de::Error),
}

/// Parse a duration string like "30s", "5m", "1h" into seconds.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('s') {
        n.parse().ok()
    } else if let Some(n) = s.strip_suffix('m') {
        n.parse::<u64>().ok().map(|n| n * 60)
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>().ok().map(|n| n * 3600)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("5m"), Some(300));
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs("60"), Some(60));
    }

    #[test]
    fn test_parse_example_service() {
        let toml_str = r#"
[service]
name = "test-svc"
exec = ["/bin/echo", "hello"]
type = "oneshot"

[restart]
policy = "temporary"

[readiness]
type = "none"
timeout = "10s"

[dependencies]
requires = []

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "5s"
"#;
        let def: ServiceDef = toml::from_str(toml_str).unwrap();
        assert_eq!(def.service.name, "test-svc");
        assert_eq!(def.service.service_type, ServiceType::Oneshot);
        assert_eq!(def.restart.policy, RestartPolicy::Temporary);
        assert_eq!(def.service.supervisor, "root");
    }
}
