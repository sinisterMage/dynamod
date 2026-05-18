/// Lid / power-button / sleep-button event source.
///
/// We listen to evdev (`/dev/input/event*`) for the canonical keycodes:
///   - KEY_POWER (116)
///   - KEY_SLEEP (142)
///   - SW_LID    (type EV_SW, code SW_LID=0)
///
/// Selecting input devices is done by reading their capability bitmasks in
/// sysfs (`/sys/class/input/event*/device/capabilities/{key,sw}`), so we only
/// open the small set of devices that can produce these events.
///
/// All raw I/O happens in a `spawn_blocking` task; events are dispatched
/// through a tokio mpsc channel into the async logind core.
use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};

use crate::config::HandleAction;
use crate::power::{self, ShutdownKind, SleepKind};
use crate::state::LoginState;

/// evdev event types / codes we care about.
const EV_KEY: u16 = 0x01;
const EV_SW: u16 = 0x05;
const KEY_POWER: u16 = 116;
const KEY_SLEEP: u16 = 142;
const SW_LID: u16 = 0x00;

#[derive(Debug, Clone, Copy)]
pub enum AcpiEvent {
    PowerButton,
    SleepButton,
    LidClosed,
    LidOpened,
}

/// Raw evdev event: `struct input_event` (24 bytes on 64-bit Linux, with
/// __kernel_old_timeval). Matches the kernel UAPI for the lifetime of this
/// codebase.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputEvent {
    tv_sec: i64,
    tv_usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

const INPUT_EVENT_SIZE: usize = std::mem::size_of::<InputEvent>();

/// Spawn the ACPI event pipeline. The dispatch loop takes ownership of
/// `state` (cloned Arc) and a callable that emits PrepareForSleep/Shutdown.
pub fn spawn(
    state: Arc<Mutex<LoginState>>,
    emit_prepare_for_sleep: PrepareEmitter,
    emit_prepare_for_shutdown: PrepareEmitter,
) {
    let (tx, rx) = mpsc::channel::<AcpiEvent>(16);

    let devices = scan_devices();
    if devices.is_empty() {
        tracing::info!(
            "acpi: no power/lid input devices found; lid and power-button handling disabled"
        );
    }

    for dev in devices {
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || read_loop(&dev, tx));
    }

    tokio::spawn(dispatch_loop(
        state,
        rx,
        emit_prepare_for_sleep,
        emit_prepare_for_shutdown,
    ));
}

/// A closure that emits a `PrepareForSleep`/`PrepareForShutdown` signal.
/// Boxed because the closure captures a zbus Connection and we need to
/// pass it across await points.
pub type PrepareEmitter =
    Arc<dyn Fn(bool) -> zbus::Result<()> + Send + Sync + 'static>;

/// Find input devices that support KEY_POWER / KEY_SLEEP or SW_LID.
fn scan_devices() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let caps_dir = entry.path().join("device").join("capabilities");
        let supports_pwr_sleep = capability_has_bit(&caps_dir.join("key"), KEY_POWER)
            || capability_has_bit(&caps_dir.join("key"), KEY_SLEEP);
        let supports_lid = capability_has_bit(&caps_dir.join("sw"), SW_LID);
        if supports_pwr_sleep || supports_lid {
            out.push(PathBuf::from(format!("/dev/input/{name}")));
        }
    }
    out
}

/// Parse a sysfs capability bitmask file and check whether a given bit is set.
///
/// The file format is space-separated 64-bit hex words, **most significant
/// word first** (the kernel's `__bitmap_print_to_buf` order).
fn capability_has_bit(path: &Path, bit: u16) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let words: Vec<u64> = content
        .split_ascii_whitespace()
        .rev()
        .filter_map(|w| u64::from_str_radix(w, 16).ok())
        .collect();
    let bit = bit as usize;
    let word = bit / 64;
    let off = bit % 64;
    words.get(word).map_or(false, |w| (w >> off) & 1 == 1)
}

/// Blocking read loop over a single evdev device. Sends abstracted
/// `AcpiEvent`s on the channel; exits if the device disappears.
fn read_loop(path: &Path, tx: mpsc::Sender<AcpiEvent>) {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("acpi: cannot open {}: {e}", path.display());
            return;
        }
    };
    tracing::info!("acpi: listening on {}", path.display());

    let mut buf = [0u8; INPUT_EVENT_SIZE];
    loop {
        if let Err(e) = file.read_exact(&mut buf) {
            tracing::warn!("acpi: read from {} failed: {e}", path.display());
            return;
        }
        let ev: InputEvent = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const _) };
        if let Some(event) = classify(&ev) {
            // Use blocking_send so we don't drop events under load.
            if tx.blocking_send(event).is_err() {
                return;
            }
        }
    }
}

fn classify(ev: &InputEvent) -> Option<AcpiEvent> {
    match (ev.type_, ev.code, ev.value) {
        (EV_KEY, KEY_POWER, 1) => Some(AcpiEvent::PowerButton),
        (EV_KEY, KEY_SLEEP, 1) => Some(AcpiEvent::SleepButton),
        (EV_SW, SW_LID, 1) => Some(AcpiEvent::LidClosed),
        (EV_SW, SW_LID, 0) => Some(AcpiEvent::LidOpened),
        _ => None,
    }
}

async fn dispatch_loop(
    state: Arc<Mutex<LoginState>>,
    mut rx: mpsc::Receiver<AcpiEvent>,
    emit_prepare_for_sleep: PrepareEmitter,
    emit_prepare_for_shutdown: PrepareEmitter,
) {
    let mut last_lid_event = std::time::Instant::now() - Duration::from_secs(3600);

    while let Some(event) = rx.recv().await {
        let cfg = state.lock().await.config.read().await.clone();
        let (action, debounce) = match event {
            AcpiEvent::PowerButton => (cfg.handle_power_key, false),
            AcpiEvent::SleepButton => (cfg.handle_suspend_key, false),
            AcpiEvent::LidClosed => (
                if cfg.handle_lid_switch_docked != HandleAction::Ignore
                    && is_docked()
                {
                    cfg.handle_lid_switch_docked
                } else if on_external_power() {
                    cfg.handle_lid_switch_external_power
                } else {
                    cfg.handle_lid_switch
                },
                true,
            ),
            AcpiEvent::LidOpened => continue,
        };

        if debounce {
            let now = std::time::Instant::now();
            if now.duration_since(last_lid_event) < cfg.holdoff_timeout {
                tracing::debug!("acpi: lid event debounced");
                continue;
            }
            last_lid_event = now;
        }

        run_action(&state, action, &emit_prepare_for_sleep, &emit_prepare_for_shutdown)
            .await;
    }
}

async fn run_action(
    state: &Arc<Mutex<LoginState>>,
    action: HandleAction,
    emit_prepare_for_sleep: &PrepareEmitter,
    emit_prepare_for_shutdown: &PrepareEmitter,
) {
    let what = action.inhibit_what();
    if !what.is_empty() && power::has_block_inhibitor(state, what).await {
        tracing::info!("acpi: action {action:?} blocked by inhibitor for '{what}'");
        return;
    }

    match action {
        HandleAction::Ignore | HandleAction::Lock => {}
        HandleAction::Suspend => {
            let emit = emit_prepare_for_sleep.clone();
            let _ = power::execute_sleep(
                Arc::clone(state),
                move |on| emit(on),
                SleepKind::Suspend,
            )
            .await;
        }
        HandleAction::Hibernate => {
            let emit = emit_prepare_for_sleep.clone();
            let _ = power::execute_sleep(
                Arc::clone(state),
                move |on| emit(on),
                SleepKind::Hibernate,
            )
            .await;
        }
        HandleAction::HybridSleep => {
            let emit = emit_prepare_for_sleep.clone();
            let _ = power::execute_sleep(
                Arc::clone(state),
                move |on| emit(on),
                SleepKind::HybridSleep,
            )
            .await;
        }
        HandleAction::Poweroff => {
            let emit = emit_prepare_for_shutdown.clone();
            let _ = power::execute_shutdown(
                Arc::clone(state),
                move |on| emit(on),
                ShutdownKind::Poweroff,
            )
            .await;
        }
        HandleAction::Reboot => {
            let emit = emit_prepare_for_shutdown.clone();
            let _ = power::execute_shutdown(
                Arc::clone(state),
                move |on| emit(on),
                ShutdownKind::Reboot,
            )
            .await;
        }
        HandleAction::Halt => {
            let emit = emit_prepare_for_shutdown.clone();
            let _ = power::execute_shutdown(
                Arc::clone(state),
                move |on| emit(on),
                ShutdownKind::Halt,
            )
            .await;
        }
    }
}

fn is_docked() -> bool {
    // Approximation — systemd reads ACPI dock state too. Stubbed for now.
    false
}

fn on_external_power() -> bool {
    fs::read_to_string("/sys/class/power_supply/AC/online")
        .map(|s| s.trim() == "1")
        .unwrap_or(true)
}

// Extension trait for OpenOptions on Linux to set O_CLOEXEC alongside read mode.
use std::os::unix::fs::OpenOptionsExt;

#[allow(dead_code)]
fn _ensure_imports_used(_: &dyn AsRawFd) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bitmap_parsing() {
        // simulate: bit 116 (KEY_POWER) set, bit 142 (KEY_SLEEP) set.
        // bit 116 -> word index 1, bit 116-64=52
        // bit 142 -> word index 2, bit 142-128=14
        let tmp = std::env::temp_dir().join(format!(
            "dynamod-acpi-test-{}.bits",
            std::process::id()
        ));
        let word0: u64 = 0;
        let word1: u64 = 1u64 << 52;
        let word2: u64 = 1u64 << 14;
        // MSB-first order
        let content = format!("{word2:x} {word1:x} {word0:x}");
        std::fs::write(&tmp, content).unwrap();

        assert!(capability_has_bit(&tmp, KEY_POWER));
        assert!(capability_has_bit(&tmp, KEY_SLEEP));
        assert!(!capability_has_bit(&tmp, 200));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn classifier_recognizes_canonical_events() {
        let pwr = InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: EV_KEY,
            code: KEY_POWER,
            value: 1,
        };
        assert!(matches!(classify(&pwr), Some(AcpiEvent::PowerButton)));

        let lid_close = InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: EV_SW,
            code: SW_LID,
            value: 1,
        };
        assert!(matches!(classify(&lid_close), Some(AcpiEvent::LidClosed)));

        // Key release (value=0) is ignored.
        let pwr_release = InputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_: EV_KEY,
            code: KEY_POWER,
            value: 0,
        };
        assert!(classify(&pwr_release).is_none());
    }
}
