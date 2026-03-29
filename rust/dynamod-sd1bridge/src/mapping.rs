/// Translation between dynamod service model and systemd unit model.

/// Strip the `.service` suffix from a systemd unit name to get the
/// dynamod service name.
pub fn unit_to_service(unit_name: &str) -> &str {
    unit_name
        .strip_suffix(".service")
        .unwrap_or(unit_name)
}

/// Append `.service` to a dynamod service name to get a systemd unit name.
pub fn service_to_unit(service_name: &str) -> String {
    if service_name.ends_with(".service") {
        service_name.to_string()
    } else {
        format!("{service_name}.service")
    }
}

/// Map dynamod ServiceStatus to systemd (ActiveState, SubState).
pub fn map_status(dynamod_status: &str) -> (&'static str, &'static str) {
    match dynamod_status {
        "running" => ("active", "running"),
        "stopped" => ("inactive", "dead"),
        "starting" => ("activating", "start"),
        "waiting-ready" => ("activating", "start"),
        "stopping" => ("deactivating", "stop"),
        "failed" => ("failed", "failed"),
        "abandoned" => ("failed", "abandoned"),
        _ => ("inactive", "dead"),
    }
}

/// Map dynamod status to systemd LoadState.
pub fn load_state(_dynamod_status: &str) -> &'static str {
    "loaded"
}

/// Escape a unit name for use in D-Bus object paths.
/// systemd uses a specific escaping scheme for unit names.
pub fn escape_unit_path(unit_name: &str) -> String {
    let mut out = String::new();
    for b in unit_name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("_{:02x}", b));
        }
    }
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

/// Build the D-Bus object path for a unit.
pub fn unit_object_path(unit_name: &str) -> String {
    format!(
        "/org/freedesktop/systemd1/unit/{}",
        escape_unit_path(unit_name)
    )
}
