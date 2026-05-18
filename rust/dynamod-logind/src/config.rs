/// /etc/dynamod/logind.conf parser.
///
/// INI-style key=value lines, `#` and `;` comments, drop-in compatible with
/// systemd's logind.conf(5) keys so porters can copy stanzas verbatim.
use std::fs;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_PATH: &str = "/etc/dynamod/logind.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleAction {
    Ignore,
    Poweroff,
    Reboot,
    Halt,
    Suspend,
    Hibernate,
    HybridSleep,
    Lock,
}

impl HandleAction {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "ignore" => Self::Ignore,
            "poweroff" => Self::Poweroff,
            "reboot" => Self::Reboot,
            "halt" => Self::Halt,
            "suspend" => Self::Suspend,
            "hibernate" => Self::Hibernate,
            "hybrid-sleep" => Self::HybridSleep,
            "lock" => Self::Lock,
            _ => return None,
        })
    }

    /// The "what" string used to match block-inhibitors.
    pub fn inhibit_what(self) -> &'static str {
        match self {
            Self::Poweroff | Self::Reboot | Self::Halt => "shutdown",
            Self::Suspend | Self::HybridSleep => "sleep",
            Self::Hibernate => "sleep",
            Self::Lock | Self::Ignore => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub handle_power_key: HandleAction,
    pub handle_suspend_key: HandleAction,
    pub handle_hibernate_key: HandleAction,
    pub handle_lid_switch: HandleAction,
    pub handle_lid_switch_external_power: HandleAction,
    pub handle_lid_switch_docked: HandleAction,

    pub n_auto_vts: u32,
    pub kill_user_processes: bool,
    pub inhibit_delay_max: Duration,
    pub user_stop_delay: Duration,
    pub holdoff_timeout: Duration,
    pub runtime_directory_size: String,

    pub idle_action: HandleAction,
    pub idle_action_sec: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handle_power_key: HandleAction::Poweroff,
            handle_suspend_key: HandleAction::Suspend,
            handle_hibernate_key: HandleAction::Hibernate,
            handle_lid_switch: HandleAction::Suspend,
            handle_lid_switch_external_power: HandleAction::Suspend,
            handle_lid_switch_docked: HandleAction::Ignore,
            n_auto_vts: 6,
            kill_user_processes: false,
            inhibit_delay_max: Duration::from_secs(5),
            user_stop_delay: Duration::from_secs(10),
            holdoff_timeout: Duration::from_secs(30),
            runtime_directory_size: "10%".to_string(),
            idle_action: HandleAction::Ignore,
            idle_action_sec: Duration::from_secs(30 * 60),
        }
    }
}

impl Config {
    /// Load config from the default path. Returns defaults if the file is
    /// absent or unreadable.
    pub fn load() -> Self {
        Self::load_from(Path::new(DEFAULT_PATH))
    }

    pub fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => {
                let mut cfg = Self::default();
                cfg.merge(&s);
                cfg
            }
            Err(e) => {
                tracing::info!("no logind.conf at {} ({}); using defaults", path.display(), e);
                Self::default()
            }
        }
    }

    /// Merge keys from the given INI text into self.
    fn merge(&mut self, text: &str) {
        for (lineno, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            // Skip [Section] headers — systemd uses [Login] but we accept all.
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                tracing::warn!("logind.conf line {}: no '=', ignoring", lineno + 1);
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            self.apply(key, value, lineno + 1);
        }
    }

    fn apply(&mut self, key: &str, value: &str, lineno: usize) {
        let warn_unknown = |what: &str| {
            tracing::warn!("logind.conf line {lineno}: unknown value '{value}' for {what}");
        };

        match key {
            "HandlePowerKey" => match HandleAction::parse(value) {
                Some(a) => self.handle_power_key = a,
                None => warn_unknown(key),
            },
            "HandleSuspendKey" => match HandleAction::parse(value) {
                Some(a) => self.handle_suspend_key = a,
                None => warn_unknown(key),
            },
            "HandleHibernateKey" => match HandleAction::parse(value) {
                Some(a) => self.handle_hibernate_key = a,
                None => warn_unknown(key),
            },
            "HandleLidSwitch" => match HandleAction::parse(value) {
                Some(a) => self.handle_lid_switch = a,
                None => warn_unknown(key),
            },
            "HandleLidSwitchExternalPower" => match HandleAction::parse(value) {
                Some(a) => self.handle_lid_switch_external_power = a,
                None => warn_unknown(key),
            },
            "HandleLidSwitchDocked" => match HandleAction::parse(value) {
                Some(a) => self.handle_lid_switch_docked = a,
                None => warn_unknown(key),
            },
            "IdleAction" => match HandleAction::parse(value) {
                Some(a) => self.idle_action = a,
                None => warn_unknown(key),
            },
            "NAutoVTs" => match value.parse::<u32>() {
                Ok(n) => self.n_auto_vts = n,
                Err(_) => warn_unknown(key),
            },
            "KillUserProcesses" => match parse_bool(value) {
                Some(b) => self.kill_user_processes = b,
                None => warn_unknown(key),
            },
            "InhibitDelayMaxSec" => match parse_duration_sec(value) {
                Some(d) => self.inhibit_delay_max = d,
                None => warn_unknown(key),
            },
            "UserStopDelaySec" => match parse_duration_sec(value) {
                Some(d) => self.user_stop_delay = d,
                None => warn_unknown(key),
            },
            "HoldoffTimeoutSec" => match parse_duration_sec(value) {
                Some(d) => self.holdoff_timeout = d,
                None => warn_unknown(key),
            },
            "IdleActionSec" => match parse_duration_sec(value) {
                Some(d) => self.idle_action_sec = d,
                None => warn_unknown(key),
            },
            "RuntimeDirectorySize" => {
                self.runtime_directory_size = value.to_string();
            }
            _ => {
                tracing::debug!("logind.conf line {lineno}: ignoring unknown key '{key}'");
            }
        }
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Parse a systemd-style time value. We accept either a bare integer (seconds),
/// or a value with a unit suffix: `s`, `min`, `h`. The systemd grammar is
/// richer (e.g. `5min 30s`) but this covers the cases that matter for logind.
fn parse_duration_sec(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Some(Duration::from_secs(n));
    }
    if let Some(num) = s.strip_suffix("min") {
        return num.trim().parse::<u64>().ok().map(|n| Duration::from_secs(n * 60));
    }
    if let Some(num) = s.strip_suffix('h') {
        return num.trim().parse::<u64>().ok().map(|n| Duration::from_secs(n * 3600));
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_handle_actions() {
        let mut cfg = Config::default();
        cfg.merge(
            r#"
            # comment
            [Login]
            HandleLidSwitch=ignore
            HandlePowerKey=poweroff
            HandleSuspendKey=hybrid-sleep
            "#,
        );
        assert_eq!(cfg.handle_lid_switch, HandleAction::Ignore);
        assert_eq!(cfg.handle_power_key, HandleAction::Poweroff);
        assert_eq!(cfg.handle_suspend_key, HandleAction::HybridSleep);
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration_sec("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration_sec("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration_sec("2min"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration_sec("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration_sec("garbage"), None);
    }

    #[test]
    fn parses_bools() {
        assert_eq!(parse_bool("yes"), Some(true));
        assert_eq!(parse_bool("NO"), Some(false));
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn defaults_preserved_on_unknown_value() {
        let mut cfg = Config::default();
        cfg.merge("HandleLidSwitch=banana\nNAutoVTs=4\n");
        assert_eq!(cfg.handle_lid_switch, HandleAction::Suspend); // default
        assert_eq!(cfg.n_auto_vts, 4);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let mut cfg = Config::default();
        cfg.merge("NotARealKey=blah\nHandleLidSwitch=ignore\n");
        assert_eq!(cfg.handle_lid_switch, HandleAction::Ignore);
    }

    #[test]
    fn inhibit_what_matches_systemd() {
        assert_eq!(HandleAction::Poweroff.inhibit_what(), "shutdown");
        assert_eq!(HandleAction::Suspend.inhibit_what(), "sleep");
        assert_eq!(HandleAction::Ignore.inhibit_what(), "");
    }
}
