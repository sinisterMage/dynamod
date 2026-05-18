/// Power-action handshake.
///
/// Wraps suspend/shutdown actions with the systemd-compatible signal protocol
/// that desktops like Plasma expect:
///   1. Emit `PrepareForSleep(true)` / `PrepareForShutdown(true)`.
///   2. Wait for outstanding *delay* inhibitors to release, up to
///      `InhibitDelayMaxSec` (default 5s).
///   3. Execute the action.
///   4. For sleep: emit `PrepareForSleep(false)` after the kernel returns
///      from `write("/sys/power/state", "mem")`.
///
/// Block inhibitors are checked at the call site (the ACPI handler and the
/// public Manager.Suspend/PowerOff/etc. methods); this module only handles the
/// delay-inhibitor wait.
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::state::LoginState;

#[derive(Debug, Clone, Copy)]
pub enum SleepKind {
    Suspend,
    Hibernate,
    HybridSleep,
}

impl SleepKind {
    pub fn sysfs_state(self) -> &'static str {
        match self {
            Self::Suspend => "mem",
            Self::Hibernate => "disk",
            Self::HybridSleep => "disk",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ShutdownKind {
    Poweroff,
    Reboot,
    Halt,
}

impl From<ShutdownKind> for dynamod_common::protocol::ShutdownKind {
    fn from(k: ShutdownKind) -> Self {
        match k {
            ShutdownKind::Poweroff => Self::Poweroff,
            ShutdownKind::Reboot => Self::Reboot,
            ShutdownKind::Halt => Self::Halt,
        }
    }
}

/// Wait for delay-inhibitors for `what` to drain, but no longer than `max`.
async fn wait_for_delay_inhibitors(
    state: &Arc<Mutex<LoginState>>,
    what: &str,
    max: Duration,
) {
    let deadline = std::time::Instant::now() + max;
    loop {
        // GC released inhibitors, then check.
        let still_blocking = {
            let mut st = state.lock().await;
            st.inhibitors.retain(|i| !i.is_released());
            st.inhibitors
                .iter()
                .any(|i| i.mode == "delay" && i.what.split(':').any(|w| w == what))
        };
        if !still_blocking {
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                "delay-inhibitor wait for '{what}' exceeded {:?}; proceeding anyway",
                max
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Execute a sleep action with the prepare/resume handshake.
pub async fn execute_sleep(
    state: Arc<Mutex<LoginState>>,
    emit_prepare_for_sleep: impl FnOnce(bool) -> zbus::Result<()> + Clone,
    kind: SleepKind,
) -> Result<(), String> {
    let max = state.lock().await.config.read().await.inhibit_delay_max;

    let _ = (emit_prepare_for_sleep.clone())(true);
    wait_for_delay_inhibitors(&state, "sleep", max).await;

    let sysfs_state = kind.sysfs_state().to_string();
    let result = tokio::task::spawn_blocking(move || {
        std::fs::write("/sys/power/state", &sysfs_state)
    })
    .await
    .map_err(|e| e.to_string())?;

    // PrepareForSleep(false) is emitted whether or not the kernel actually
    // suspended — the kernel write returns immediately with EINVAL on
    // unsupported states, and we still need to release inhibitors waiting on
    // the resume signal.
    let _ = emit_prepare_for_sleep(false);

    result.map_err(|e| e.to_string())
}

/// Execute a shutdown action with the prepare handshake.
pub async fn execute_shutdown(
    state: Arc<Mutex<LoginState>>,
    emit_prepare_for_shutdown: impl FnOnce(bool) -> zbus::Result<()>,
    kind: ShutdownKind,
) -> Result<(), String> {
    let max = state.lock().await.config.read().await.inhibit_delay_max;

    let _ = emit_prepare_for_shutdown(true);
    wait_for_delay_inhibitors(&state, "shutdown", max).await;

    let svmgr_kind: dynamod_common::protocol::ShutdownKind = kind.into();
    tokio::task::spawn_blocking(move || crate::svmgr_client::request_shutdown(svmgr_kind))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Check whether any block-inhibitor would prevent the given action.
pub async fn has_block_inhibitor(state: &Arc<Mutex<LoginState>>, what: &str) -> bool {
    let mut st = state.lock().await;
    st.inhibitors.retain(|i| !i.is_released());
    st.inhibitors
        .iter()
        .any(|i| i.mode == "block" && i.what.split(':').any(|w| w == what))
}
