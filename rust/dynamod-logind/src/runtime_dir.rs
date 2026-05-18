/// Per-user XDG runtime directory lifecycle.
///
/// Mirrors what `pam_systemd.so` does at session-open: ensure /run/user/$UID
/// exists, owned by the user, mounted tmpfs with mode 0700. Removed (with a
/// grace period) when the user's last session closes.
use std::fs;
use std::io;
use std::os::unix::fs::{chown, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::mount::{mount, umount2, MntFlags, MsFlags};

pub fn path_for(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}"))
}

/// Create /run/user/$UID owned by uid:gid with mode 0700, and mount tmpfs on
/// top of it if not already mounted. Idempotent.
pub fn ensure(uid: u32, gid: u32, size: &str) -> io::Result<()> {
    let path = path_for(uid);

    // /run might not exist in container test environments — best-effort mkdir.
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if !path.exists() {
        fs::create_dir(&path)?;
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    chown(&path, Some(uid), Some(gid))?;

    if !is_mounted(&path)? {
        let opts = format!("mode=0700,uid={uid},gid={gid},size={size}");
        match mount(
            Some("tmpfs"),
            &path,
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some(opts.as_str()),
        ) {
            Ok(()) => {
                // After mount the dir owner is root again until we chown the new mount.
                let _ = chown(&path, Some(uid), Some(gid));
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
            }
            Err(e) => {
                tracing::warn!(
                    "could not mount tmpfs on {}: {} (continuing without tmpfs)",
                    path.display(),
                    e
                );
            }
        }
    }
    Ok(())
}

/// Unmount and remove /run/user/$UID. Errors are logged but not propagated —
/// the dir might be busy (lingering process), in which case we leave it.
pub fn release(uid: u32) {
    let path = path_for(uid);
    if !path.exists() {
        return;
    }
    if let Err(e) = umount2(&path, MntFlags::MNT_DETACH) {
        tracing::debug!("umount {} failed: {} (likely not a mount)", path.display(), e);
    }
    if let Err(e) = fs::remove_dir(&path) {
        tracing::debug!("rmdir {} failed: {}", path.display(), e);
    }
}

fn is_mounted(path: &Path) -> io::Result<bool> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let target_str = target.to_string_lossy();
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    for line in mountinfo.lines() {
        let mut fields = line.split_whitespace();
        let mount_point = fields.nth(4);
        if mount_point == Some(target_str.as_ref()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_for_renders_uid() {
        assert_eq!(path_for(1000), PathBuf::from("/run/user/1000"));
        assert_eq!(path_for(0), PathBuf::from("/run/user/0"));
    }
}
