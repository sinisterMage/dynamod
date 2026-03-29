/// org.freedesktop.timedate1 D-Bus interface.
///
/// Manages timezone, RTC mode, and NTP settings.
/// Used by GNOME Settings "Date & Time" panel.
///
/// Clean-room implementation from freedesktop.org specs.
use zbus::{fdo, interface};

pub struct TimedateService;

#[interface(name = "org.freedesktop.timedate1")]
impl TimedateService {
    async fn set_timezone(
        &self,
        timezone: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        // Validate timezone exists
        let tz_path = format!("/usr/share/zoneinfo/{timezone}");
        if !std::path::Path::new(&tz_path).exists() {
            return Err(fdo::Error::InvalidArgs(format!(
                "timezone '{timezone}' not found"
            )));
        }

        // Remove old symlink and create new one
        let _ = std::fs::remove_file("/etc/localtime");
        std::os::unix::fs::symlink(&tz_path, "/etc/localtime")
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        // Write to /etc/timezone for compatibility
        let _ = std::fs::write("/etc/timezone", format!("{timezone}\n"));

        tracing::info!("timezone set to '{timezone}'");
        Ok(())
    }

    async fn set_local_rtc(
        &self,
        local_rtc: bool,
        _fix_system: bool,
        _interactive: bool,
    ) -> fdo::Result<()> {
        // Update /etc/adjtime
        let adjtime = if local_rtc {
            "0.0 0 0.0\n0\nLOCAL\n"
        } else {
            "0.0 0 0.0\n0\nUTC\n"
        };
        std::fs::write("/etc/adjtime", adjtime)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        tracing::info!("RTC mode set to {}", if local_rtc { "LOCAL" } else { "UTC" });
        Ok(())
    }

    async fn set_time(
        &self,
        usec_utc: i64,
        _relative: bool,
        _interactive: bool,
    ) -> fdo::Result<()> {
        let secs = usec_utc / 1_000_000;
        let usecs = (usec_utc % 1_000_000) as i64;
        let tv = libc::timeval {
            tv_sec: secs as libc::time_t,
            tv_usec: usecs as libc::suseconds_t,
        };
        let ret = unsafe { libc::settimeofday(&tv, std::ptr::null()) };
        if ret < 0 {
            return Err(fdo::Error::Failed(format!(
                "settimeofday: {}",
                std::io::Error::last_os_error()
            )));
        }
        tracing::info!("system time set");
        Ok(())
    }

    async fn set_ntp(
        &self,
        enabled: bool,
        _interactive: bool,
    ) -> fdo::Result<()> {
        // Try to start/stop common NTP services
        if enabled {
            tracing::info!("NTP enabled (no-op: delegate to NTP service)");
        } else {
            tracing::info!("NTP disabled (no-op: delegate to NTP service)");
        }
        Ok(())
    }

    // --- Properties ---

    #[zbus(property)]
    async fn timezone(&self) -> String {
        // Read symlink target of /etc/localtime
        std::fs::read_link("/etc/localtime")
            .ok()
            .and_then(|p| {
                p.to_str()?
                    .strip_prefix("/usr/share/zoneinfo/")
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                std::fs::read_to_string("/etc/timezone")
                    .ok()
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "UTC".to_string())
    }

    #[zbus(property, name = "LocalRTC")]
    async fn local_rtc(&self) -> bool {
        std::fs::read_to_string("/etc/adjtime")
            .map(|s| s.contains("LOCAL"))
            .unwrap_or(false)
    }

    #[zbus(property, name = "CanNTP")]
    async fn can_ntp(&self) -> bool {
        true
    }

    #[zbus(property, name = "NTP")]
    async fn ntp(&self) -> bool {
        // Check if any NTP client is running
        std::fs::read_dir("/run")
            .map(|entries| {
                entries.filter_map(|e| e.ok()).any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.contains("ntpd") || name.contains("chrony") || name.contains("timesyncd")
                })
            })
            .unwrap_or(false)
    }

    #[zbus(property, name = "NTPSynchronized")]
    async fn ntp_synchronized(&self) -> bool {
        // Check adjtimex status
        let mut tx: libc::timex = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::adjtimex(&mut tx) };
        // TIME_OK (0) means synchronized
        ret == 0
    }

    #[zbus(property, name = "TimeUSec")]
    async fn time_usec(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    #[zbus(property, name = "RTCTimeUSec")]
    async fn rtc_time_usec(&self) -> u64 {
        // Read RTC time from /dev/rtc0
        // This is approximate - proper implementation would use ioctl
        self.time_usec().await
    }
}
