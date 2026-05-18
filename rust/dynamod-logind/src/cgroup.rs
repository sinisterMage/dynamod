/// Session-scope cgroup placement.
///
/// Creates the systemd-canonical hierarchy
/// `/sys/fs/cgroup/user.slice/user-$UID.slice/session-$N.scope/` and writes the
/// session leader's PID into it. This is what libsystemd's
/// `sd_pid_get_session()` walks for when KWin (and other systemd-aware Wayland
/// compositors) ask "what session am I in?". Without this, libsystemd returns
/// ENOENT, no session is recognized, and the kernel refuses drmSetMaster() for
/// the unprivileged compositor.
///
/// Lives alongside dynamod-svmgr's `/sys/fs/cgroup/dynamod/` tree — both
/// enable controllers in the root subtree_control (idempotently).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const CONTROLLERS: &[&str] = &["cpu", "memory", "io", "pids"];

pub fn user_slice(uid: u32) -> PathBuf {
    PathBuf::from(format!("{CGROUP_ROOT}/user.slice/user-{uid}.slice"))
}

pub fn session_scope(uid: u32, session_id: &str) -> PathBuf {
    user_slice(uid).join(format!("session-{session_id}.scope"))
}

/// The cgroup path as it would appear in `/proc/$pid/cgroup` (without the
/// `/sys/fs/cgroup` prefix).
pub fn session_scope_relative(uid: u32, session_id: &str) -> String {
    format!("/user.slice/user-{uid}.slice/session-{session_id}.scope")
}

/// Create the slice/scope hierarchy and place `leader_pid` in the scope.
///
/// Best-effort: every step is logged-but-not-fatal. The caller should still
/// register the session even if cgroup placement fails (we run unprivileged
/// inside containers, no /sys/fs/cgroup write access, etc.).
pub fn place_leader(uid: u32, session_id: &str, leader_pid: u32) {
    let root = Path::new(CGROUP_ROOT);
    if !root.exists() {
        tracing::debug!("cgroup root {CGROUP_ROOT} missing; skipping placement");
        return;
    }

    enable_controllers(root);

    let user_slice = user_slice(uid);
    if let Err(e) = ensure_dir(&user_slice) {
        tracing::warn!("cgroup: cannot create {}: {e}", user_slice.display());
        return;
    }
    enable_controllers(&user_slice);

    let scope = session_scope(uid, session_id);
    if let Err(e) = ensure_dir(&scope) {
        tracing::warn!("cgroup: cannot create {}: {e}", scope.display());
        return;
    }

    // Write the leader PID into the scope. The kernel moves all of leader's
    // threads — but not its existing children — into the new cgroup. This is
    // fine for our case: pam_dynamod_logind calls CreateSession before exec.
    let procs = scope.join("cgroup.procs");
    if let Err(e) = fs::write(&procs, leader_pid.to_string()) {
        tracing::warn!(
            "cgroup: cannot place pid {leader_pid} in {}: {e}",
            procs.display()
        );
    } else {
        tracing::info!(
            "cgroup: placed pid {leader_pid} in user.slice/user-{uid}.slice/session-{session_id}.scope"
        );
    }
}

/// Remove the session scope after the session is gone. Best-effort.
pub fn release_scope(uid: u32, session_id: &str) {
    let scope = session_scope(uid, session_id);
    if scope.exists() {
        if let Err(e) = fs::remove_dir(&scope) {
            tracing::debug!("cgroup: rmdir {} failed: {e}", scope.display());
        }
    }
}

/// Remove the user slice after the user's last session is gone.
pub fn release_user_slice(uid: u32) {
    let slice = user_slice(uid);
    if slice.exists() {
        if let Err(e) = fs::remove_dir(&slice) {
            tracing::debug!("cgroup: rmdir {} failed: {e}", slice.display());
        }
    }
}

fn ensure_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(path)
}

/// Write `+cpu +memory +io +pids` to `path/cgroup.subtree_control`. Each
/// controller write is independent — the kernel rejects controllers that aren't
/// available, so we tolerate per-controller failures.
fn enable_controllers(path: &Path) {
    let sub = path.join("cgroup.subtree_control");
    if !sub.exists() {
        return;
    }
    for ctrl in CONTROLLERS {
        let _ = fs::write(&sub, format!("+{ctrl}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_systemd_layout() {
        assert_eq!(
            session_scope(1000, "1"),
            PathBuf::from("/sys/fs/cgroup/user.slice/user-1000.slice/session-1.scope")
        );
        assert_eq!(
            session_scope_relative(1000, "1"),
            "/user.slice/user-1000.slice/session-1.scope"
        );
        assert_eq!(
            user_slice(0),
            PathBuf::from("/sys/fs/cgroup/user.slice/user-0.slice")
        );
    }
}
