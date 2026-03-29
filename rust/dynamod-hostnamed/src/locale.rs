/// org.freedesktop.locale1 D-Bus interface.
///
/// Manages system locale and keyboard layout settings.
/// Used by GNOME Settings "Region & Language" and "Keyboard" panels.
///
/// Clean-room implementation from freedesktop.org specs.
use zbus::{fdo, interface};

pub struct LocaleService;

#[interface(name = "org.freedesktop.locale1")]
impl LocaleService {
    async fn set_locale(
        &self,
        locale: Vec<String>,
        _interactive: bool,
    ) -> fdo::Result<()> {
        let content: String = locale
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write("/etc/locale.conf", content)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        tracing::info!("locale updated");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn set_vc_console_keyboard(
        &self,
        keymap: &str,
        keymap_toggle: &str,
        _convert: bool,
        _interactive: bool,
    ) -> fdo::Result<()> {
        let mut content = format!("KEYMAP={keymap}\n");
        if !keymap_toggle.is_empty() {
            content.push_str(&format!("KEYMAP_TOGGLE={keymap_toggle}\n"));
        }
        std::fs::write("/etc/vconsole.conf", content)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        // Apply to running console
        let _ = std::process::Command::new("loadkeys")
            .arg(keymap)
            .status();

        tracing::info!("vconsole keymap set to '{keymap}'");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn set_x11_keyboard(
        &self,
        layout: &str,
        model: &str,
        variant: &str,
        options: &str,
        _convert: bool,
        _interactive: bool,
    ) -> fdo::Result<()> {
        // Write X11 keyboard configuration
        let config = format!(
            r#"# Written by dynamod-hostnamed
Section "InputClass"
    Identifier "keyboard-all"
    MatchIsKeyboard "on"
    Option "XkbLayout" "{layout}"
    Option "XkbModel" "{model}"
    Option "XkbVariant" "{variant}"
    Option "XkbOptions" "{options}"
EndSection
"#
        );

        let dir = "/etc/X11/xorg.conf.d";
        let _ = std::fs::create_dir_all(dir);
        std::fs::write(format!("{dir}/00-keyboard.conf"), config)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        tracing::info!("X11 keyboard layout set to '{layout}'");
        Ok(())
    }

    // --- Properties ---

    #[zbus(property)]
    async fn locale(&self) -> Vec<String> {
        read_locale_conf()
    }

    #[zbus(property, name = "X11Layout")]
    async fn x11_layout(&self) -> String {
        read_x11_keyboard_field("XkbLayout").unwrap_or_default()
    }

    #[zbus(property, name = "X11Model")]
    async fn x11_model(&self) -> String {
        read_x11_keyboard_field("XkbModel").unwrap_or_default()
    }

    #[zbus(property, name = "X11Variant")]
    async fn x11_variant(&self) -> String {
        read_x11_keyboard_field("XkbVariant").unwrap_or_default()
    }

    #[zbus(property, name = "X11Options")]
    async fn x11_options(&self) -> String {
        read_x11_keyboard_field("XkbOptions").unwrap_or_default()
    }

    #[zbus(property, name = "VConsoleKeymap")]
    async fn vconsole_keymap(&self) -> String {
        read_vconsole_field("KEYMAP").unwrap_or_default()
    }

    #[zbus(property, name = "VConsoleKeymapToggle")]
    async fn vconsole_keymap_toggle(&self) -> String {
        read_vconsole_field("KEYMAP_TOGGLE").unwrap_or_default()
    }
}

// --- helpers ---

fn read_locale_conf() -> Vec<String> {
    std::fs::read_to_string("/etc/locale.conf")
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

fn read_vconsole_field(key: &str) -> Option<String> {
    let content = std::fs::read_to_string("/etc/vconsole.conf").ok()?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix(&format!("{key}=")) {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

fn read_x11_keyboard_field(key: &str) -> Option<String> {
    let content =
        std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf").ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("Option") && line.contains(key) {
            // Parse: Option "XkbLayout" "us"
            let parts: Vec<&str> = line.split('"').collect();
            if parts.len() >= 4 {
                return Some(parts[3].to_string());
            }
        }
    }
    None
}
