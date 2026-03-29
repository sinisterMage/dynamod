use serde::Deserialize;
use std::path::Path;

use super::service::ServiceLoadError;

/// A parsed supervisor definition from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct SupervisorDef {
    pub supervisor: SupervisorSection,
    #[serde(default)]
    pub restart: SupervisorRestartSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SupervisorSection {
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default = "default_strategy")]
    pub strategy: RestartStrategy,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartStrategy {
    OneForOne,
    OneForAll,
    RestForOne,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SupervisorRestartSection {
    #[serde(rename = "max-restarts", default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(rename = "max-restart-window", default = "default_max_restart_window")]
    pub max_restart_window: String,
}

fn default_strategy() -> RestartStrategy {
    RestartStrategy::OneForOne
}
fn default_max_restarts() -> u32 {
    10
}
fn default_max_restart_window() -> String {
    "300s".into()
}

impl Default for SupervisorRestartSection {
    fn default() -> Self {
        Self {
            max_restarts: default_max_restarts(),
            max_restart_window: default_max_restart_window(),
        }
    }
}

/// Load a supervisor definition from a TOML file.
pub fn load_supervisor(path: &Path) -> Result<SupervisorDef, ServiceLoadError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ServiceLoadError::Io(path.display().to_string(), e))?;
    let def: SupervisorDef = toml::from_str(&content)
        .map_err(|e| ServiceLoadError::Parse(path.display().to_string(), e))?;
    Ok(def)
}

/// Load all supervisor definitions from a directory.
pub fn load_supervisors_dir(dir: &Path) -> Result<Vec<SupervisorDef>, ServiceLoadError> {
    let mut supervisors = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(supervisors),
        Err(e) => return Err(ServiceLoadError::Io(dir.display().to_string(), e)),
    };

    for entry in entries {
        let entry = entry.map_err(|e| ServiceLoadError::Io(dir.display().to_string(), e))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            match load_supervisor(&path) {
                Ok(def) => supervisors.push(def),
                Err(e) => {
                    tracing::warn!("skipping supervisor {}: {e}", path.display());
                }
            }
        }
    }

    Ok(supervisors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_supervisor() {
        let toml_str = r#"
[supervisor]
name = "root"
strategy = "one-for-one"

[restart]
max-restarts = 20
max-restart-window = "600s"
"#;
        let def: SupervisorDef = toml::from_str(toml_str).unwrap();
        assert_eq!(def.supervisor.name, "root");
        assert_eq!(def.supervisor.strategy, RestartStrategy::OneForOne);
        assert_eq!(def.restart.max_restarts, 20);
    }
}
