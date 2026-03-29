/// Service lifecycle state machine.
///
/// Tracks the state of each service through its lifecycle:
/// Stopped -> Starting -> Running -> Stopping -> Stopped/Failed
use std::time::Instant;

/// The current state of a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// Not running, never started or cleanly stopped.
    Stopped,
    /// Process has been forked, waiting for readiness.
    Starting,
    /// Running and ready.
    Running,
    /// Stop signal sent, waiting for exit.
    Stopping { deadline: Option<Instant> },
    /// Exited with failure.
    Failed {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    /// Supervisor gave up trying to restart this service.
    Abandoned,
}

impl ServiceState {
    pub fn is_running(&self) -> bool {
        matches!(self, ServiceState::Running | ServiceState::Starting)
    }

    pub fn is_stopped(&self) -> bool {
        matches!(
            self,
            ServiceState::Stopped | ServiceState::Failed { .. } | ServiceState::Abandoned
        )
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Stopping { .. } => "stopping",
            ServiceState::Failed { .. } => "failed",
            ServiceState::Abandoned => "abandoned",
        }
    }
}

/// Exit information from a child process.
#[derive(Debug, Clone)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn is_normal(&self) -> bool {
        self.exit_code == Some(0)
    }
}
