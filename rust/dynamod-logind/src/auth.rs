/// Authorization stubs for login1 methods.
///
/// Phase 1: allows everything for users with an active session.
/// Phase 2 (future): integrate with polkit via D-Bus.

/// Check whether the caller is authorized for a power action
/// (shutdown, reboot, suspend, hibernate).
///
/// Returns one of: "yes", "challenge", "na".
pub fn check_power_action(_uid: u32, _action: &str) -> &'static str {
    // TODO: polkit integration
    "yes"
}

/// Check whether the caller may modify session/seat state.
pub fn check_session_action(_uid: u32, _session_uid: u32) -> bool {
    // Allow if caller is root or owns the session
    // For now, allow everything
    true
}
