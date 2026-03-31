use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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

impl From<String> for ServiceName {
    fn from(s: String) -> Self {
        ServiceName(s)
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

impl FromStr for ServiceStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stopped" => Ok(ServiceStatus::Stopped),
            "starting" => Ok(ServiceStatus::Starting),
            "waiting-ready" => Ok(ServiceStatus::WaitingReady),
            "running" => Ok(ServiceStatus::Running),
            "stopping" => Ok(ServiceStatus::Stopping),
            "abandoned" => Ok(ServiceStatus::Abandoned),
            other if other.starts_with("failed(") => {
                let inner = other
                    .strip_prefix("failed(")
                    .and_then(|s| s.strip_suffix(')'))
                    .unwrap_or("");
                let mut exit_code = None;
                let mut signal = None;
                for part in inner.split("signal=") {
                    if let Some(rest) = part.strip_prefix("exit=") {
                        exit_code = rest.trim_end_matches(|c: char| !c.is_ascii_digit())
                            .parse().ok();
                    } else if !part.is_empty() {
                        signal = part.trim_end_matches(|c: char| !c.is_ascii_digit())
                            .parse().ok();
                    }
                }
                Ok(ServiceStatus::Failed { exit_code, signal })
            }
            _ => Err(format!("unknown service status: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_display() {
        let name = ServiceName::from("postgresql");
        assert_eq!(format!("{name}"), "postgresql");
    }

    #[test]
    fn service_name_equality() {
        let a = ServiceName::from("sshd");
        let b = ServiceName("sshd".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn service_name_from_string() {
        let name = ServiceName::from("test-service".to_string());
        assert_eq!(name.0, "test-service");
    }

    #[test]
    fn service_status_display_simple() {
        assert_eq!(format!("{}", ServiceStatus::Stopped), "stopped");
        assert_eq!(format!("{}", ServiceStatus::Starting), "starting");
        assert_eq!(format!("{}", ServiceStatus::WaitingReady), "waiting-ready");
        assert_eq!(format!("{}", ServiceStatus::Running), "running");
        assert_eq!(format!("{}", ServiceStatus::Stopping), "stopping");
        assert_eq!(format!("{}", ServiceStatus::Abandoned), "abandoned");
    }

    #[test]
    fn service_status_display_failed_exit_code() {
        let status = ServiceStatus::Failed {
            exit_code: Some(1),
            signal: None,
        };
        assert_eq!(format!("{status}"), "failed(exit=1)");
    }

    #[test]
    fn service_status_display_failed_signal() {
        let status = ServiceStatus::Failed {
            exit_code: None,
            signal: Some(9),
        };
        assert_eq!(format!("{status}"), "failed(signal=9)");
    }

    #[test]
    fn service_status_display_failed_both() {
        let status = ServiceStatus::Failed {
            exit_code: Some(1),
            signal: Some(15),
        };
        assert_eq!(format!("{status}"), "failed(exit=1signal=15)");
    }

    #[test]
    fn service_status_serde_roundtrip() {
        let statuses = vec![
            ServiceStatus::Stopped,
            ServiceStatus::Running,
            ServiceStatus::Failed {
                exit_code: Some(127),
                signal: None,
            },
        ];
        for status in statuses {
            let serialized = rmp_serde::to_vec(&status).unwrap();
            let deserialized: ServiceStatus = rmp_serde::from_slice(&serialized).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn service_name_hash_consistent() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ServiceName::from("sshd"));
        assert!(set.contains(&ServiceName::from("sshd")));
        assert!(!set.contains(&ServiceName::from("nginx")));
    }

    #[test]
    fn service_status_from_str_simple() {
        assert_eq!("stopped".parse::<ServiceStatus>().unwrap(), ServiceStatus::Stopped);
        assert_eq!("running".parse::<ServiceStatus>().unwrap(), ServiceStatus::Running);
        assert_eq!("starting".parse::<ServiceStatus>().unwrap(), ServiceStatus::Starting);
        assert_eq!("stopping".parse::<ServiceStatus>().unwrap(), ServiceStatus::Stopping);
        assert_eq!("waiting-ready".parse::<ServiceStatus>().unwrap(), ServiceStatus::WaitingReady);
        assert_eq!("abandoned".parse::<ServiceStatus>().unwrap(), ServiceStatus::Abandoned);
    }

    #[test]
    fn service_status_from_str_failed() {
        let status: ServiceStatus = "failed(exit=1)".parse().unwrap();
        assert_eq!(
            status,
            ServiceStatus::Failed { exit_code: Some(1), signal: None }
        );
    }

    #[test]
    fn service_status_from_str_unknown() {
        assert!("bogus".parse::<ServiceStatus>().is_err());
    }

    #[test]
    fn service_status_display_roundtrip() {
        let statuses = vec![
            ServiceStatus::Stopped,
            ServiceStatus::Running,
            ServiceStatus::Starting,
            ServiceStatus::Stopping,
            ServiceStatus::WaitingReady,
            ServiceStatus::Abandoned,
        ];
        for status in statuses {
            let displayed = format!("{status}");
            let parsed: ServiceStatus = displayed.parse().unwrap();
            assert_eq!(status, parsed);
        }
    }
}
