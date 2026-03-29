/// org.freedesktop.hostname1 D-Bus interface.
///
/// Reads/writes /etc/hostname, /etc/os-release, and /sys/class/dmi/id/
/// for system identification. Used by GNOME Settings "About" panel.
///
/// Clean-room implementation from freedesktop.org specs.
use zbus::{fdo, interface};

pub struct HostnameService;

#[interface(name = "org.freedesktop.hostname1")]
impl HostnameService {
    async fn set_static_hostname(
        &self,
        hostname: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        std::fs::write("/etc/hostname", format!("{hostname}\n"))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        // Apply immediately via syscall
        let c_name = std::ffi::CString::new(hostname)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let ret = unsafe { libc::sethostname(c_name.as_ptr(), hostname.len()) };
        if ret < 0 {
            return Err(fdo::Error::Failed(format!(
                "sethostname: {}",
                std::io::Error::last_os_error()
            )));
        }
        tracing::info!("hostname set to '{hostname}'");
        Ok(())
    }

    async fn set_pretty_hostname(
        &self,
        hostname: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        // Store in /etc/machine-info
        update_machine_info("PRETTY_HOSTNAME", hostname)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn set_icon_name(
        &self,
        icon: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        update_machine_info("ICON_NAME", icon)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn set_chassis(
        &self,
        chassis: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        update_machine_info("CHASSIS", chassis)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn set_deployment(
        &self,
        deployment: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        update_machine_info("DEPLOYMENT", deployment)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn set_location(
        &self,
        location: &str,
        _interactive: bool,
    ) -> fdo::Result<()> {
        update_machine_info("LOCATION", location)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn describe(&self) -> fdo::Result<String> {
        Ok(format!(
            "hostname={}, os={}",
            read_hostname(),
            read_os_release_field("PRETTY_NAME").unwrap_or_default()
        ))
    }

    // --- Properties ---

    #[zbus(property)]
    async fn hostname(&self) -> String {
        let mut buf = [0u8; 256];
        let ret = unsafe {
            libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len())
        };
        if ret == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..len]).to_string()
        } else {
            "localhost".to_string()
        }
    }

    #[zbus(property)]
    async fn static_hostname(&self) -> String {
        read_hostname()
    }

    #[zbus(property)]
    async fn pretty_hostname(&self) -> String {
        read_machine_info_field("PRETTY_HOSTNAME").unwrap_or_default()
    }

    #[zbus(property)]
    async fn default_hostname(&self) -> String {
        "localhost".to_string()
    }

    #[zbus(property)]
    async fn hostname_source(&self) -> String {
        if std::fs::metadata("/etc/hostname").is_ok() {
            "static".to_string()
        } else {
            "default".to_string()
        }
    }

    #[zbus(property)]
    async fn icon_name(&self) -> String {
        read_machine_info_field("ICON_NAME")
            .unwrap_or_else(|| "computer".to_string())
    }

    #[zbus(property)]
    async fn chassis(&self) -> String {
        read_machine_info_field("CHASSIS")
            .or_else(|| detect_chassis())
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn deployment(&self) -> String {
        read_machine_info_field("DEPLOYMENT").unwrap_or_default()
    }

    #[zbus(property)]
    async fn location(&self) -> String {
        read_machine_info_field("LOCATION").unwrap_or_default()
    }

    #[zbus(property)]
    async fn kernel_name(&self) -> String {
        "Linux".to_string()
    }

    #[zbus(property)]
    async fn kernel_release(&self) -> String {
        read_utsname_field(|u| {
            let ptr = u.release.as_ptr();
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default()
    }

    #[zbus(property)]
    async fn kernel_version(&self) -> String {
        read_utsname_field(|u| {
            let ptr = u.version.as_ptr();
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default()
    }

    #[zbus(property, name = "OperatingSystemPrettyName")]
    async fn os_pretty_name(&self) -> String {
        read_os_release_field("PRETTY_NAME").unwrap_or_else(|| "Linux".to_string())
    }

    #[zbus(property, name = "OperatingSystemCPEName")]
    async fn os_cpe_name(&self) -> String {
        read_os_release_field("CPE_NAME").unwrap_or_default()
    }

    #[zbus(property, name = "OperatingSystemHomeURL")]
    async fn os_home_url(&self) -> String {
        read_os_release_field("HOME_URL").unwrap_or_default()
    }

    #[zbus(property)]
    async fn hardware_vendor(&self) -> String {
        read_dmi("sys_vendor").unwrap_or_default()
    }

    #[zbus(property)]
    async fn hardware_model(&self) -> String {
        read_dmi("product_name").unwrap_or_default()
    }

    #[zbus(property)]
    async fn firmware_version(&self) -> String {
        read_dmi("bios_version").unwrap_or_default()
    }

    #[zbus(property)]
    async fn firmware_vendor(&self) -> String {
        read_dmi("bios_vendor").unwrap_or_default()
    }

    #[zbus(property)]
    async fn firmware_date(&self) -> String {
        read_dmi("bios_date").unwrap_or_default()
    }
}

// --- helpers ---

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "localhost".to_string())
}

fn read_dmi(field: &str) -> Option<String> {
    std::fs::read_to_string(format!("/sys/class/dmi/id/{field}"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_os_release_field(key: &str) -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix(&format!("{key}=")) {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

fn read_machine_info_field(key: &str) -> Option<String> {
    let content = std::fs::read_to_string("/etc/machine-info").ok()?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix(&format!("{key}=")) {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

fn update_machine_info(key: &str, value: &str) -> Result<(), std::io::Error> {
    let content = std::fs::read_to_string("/etc/machine-info").unwrap_or_default();
    let mut lines: Vec<String> = content
        .lines()
        .filter(|l| !l.starts_with(&format!("{key}=")))
        .map(|l| l.to_string())
        .collect();
    if !value.is_empty() {
        lines.push(format!("{key}=\"{value}\""));
    }
    std::fs::write("/etc/machine-info", lines.join("\n") + "\n")
}

fn read_utsname_field<F: Fn(&libc::utsname) -> String>(f: F) -> Option<String> {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::uname(&mut uts) };
    if ret == 0 {
        Some(f(&uts))
    } else {
        None
    }
}

fn detect_chassis() -> Option<String> {
    // Try DMI chassis type
    let ct = std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    // Based on SMBIOS spec chassis types
    let chassis = match ct {
        3 | 4 | 5 | 6 | 7 | 15 | 16 => "desktop",
        8 | 9 | 10 | 14 => "laptop",
        11 => "handset",
        17 | 23 | 28 | 29 => "server",
        30 | 31 | 32 => "tablet",
        _ => return None,
    };
    Some(chassis.to_string())
}
