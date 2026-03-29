use serde::{Deserialize, Serialize};
use std::fmt;

/// A service name, e.g. "postgresql" or "network.dhcpcd".
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceName(pub String);

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ServiceName {
    fn from(s: &str) -> Self {
        ServiceName(s.to_string())
    }
}

/// Service status as seen by the service manager.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    WaitingReady,
    Running,
    Stopping,
    Failed { exit_code: Option<i32>, signal: Option<i32> },
    Abandoned,
}

impl fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceStatus::Stopped => write!(f, "stopped"),
            ServiceStatus::Starting => write!(f, "starting"),
            ServiceStatus::WaitingReady => write!(f, "waiting-ready"),
            ServiceStatus::Running => write!(f, "running"),
            ServiceStatus::Stopping => write!(f, "stopping"),
            ServiceStatus::Failed { exit_code, signal } => {
                write!(f, "failed(")?;
                if let Some(code) = exit_code {
                    write!(f, "exit={code}")?;
                }
                if let Some(sig) = signal {
                    write!(f, "signal={sig}")?;
                }
                write!(f, ")")
            }
            ServiceStatus::Abandoned => write!(f, "abandoned"),
        }
    }
}
