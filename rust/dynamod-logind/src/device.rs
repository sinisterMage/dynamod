/// Device management for TakeDevice / ReleaseDevice.
///
/// Opens device nodes and passes fds to session controllers (Wayland compositors).
/// Manages DRM master status during session switches.
///
/// Also handles "uaccess"-style ACL handover: when a session becomes active on
/// a seat, the DRM and input devices on that seat are chowned to the session
/// user (and reverted on deactivation). This replaces the udev `uaccess` tag
/// + ACL rules that systemd-using systems rely on; without it, an unprivileged
/// compositor that doesn't go through TakeDevice (some KDE auxiliary tools)
/// can't open `/dev/dri/card0`.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

/// DRM ioctl numbers (from linux/drm.h, public kernel UAPI).
const DRM_IOCTL_BASE: libc::Ioctl = b'd' as libc::Ioctl;
const DRM_IOCTL_SET_MASTER: libc::Ioctl = request_code_none(DRM_IOCTL_BASE, 0x1E);
const DRM_IOCTL_DROP_MASTER: libc::Ioctl = request_code_none(DRM_IOCTL_BASE, 0x1F);

/// DRM major device number.
pub const DRM_MAJOR: u32 = 226;

/// Construct an ioctl number with no argument (_IO).
const fn request_code_none(ty: libc::Ioctl, nr: libc::Ioctl) -> libc::Ioctl {
    (ty << 8) | nr
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("failed to open device {path}: {source}")]
    Open {
        path: String,
        source: std::io::Error,
    },
    #[error("DRM ioctl failed on fd {fd}: {source}")]
    DrmIoctl {
        fd: i32,
        source: nix::Error,
    },
}

/// Open a character device by major/minor number.
///
/// The device path is `/dev/char/<major>:<minor>`. If that symlink doesn't
/// exist we fall back to scanning `/sys/dev/char/<major>:<minor>/uevent` for
/// the `DEVNAME` field.
pub fn open_device(major: u32, minor: u32) -> Result<OwnedFd, DeviceError> {
    let path = format!("/dev/char/{}:{}", major, minor);

    // Try the /dev/char/ symlink first, fall back to sysfs lookup
    let actual_path = if std::fs::symlink_metadata(&path).is_ok() {
        path.clone()
    } else {
        resolve_devname(major, minor).unwrap_or(path.clone())
    };

    let fd = unsafe {
        let raw = libc::open(
            actual_path.as_ptr() as *const libc::c_char,
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK,
        );
        if raw < 0 {
            // Try with the c_str properly
            let c_path = std::ffi::CString::new(actual_path.as_bytes())
                .map_err(|_| DeviceError::Open {
                    path: actual_path.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid path",
                    ),
                })?;
            let raw = libc::open(
                c_path.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK,
            );
            if raw < 0 {
                return Err(DeviceError::Open {
                    path: actual_path,
                    source: std::io::Error::last_os_error(),
                });
            }
            OwnedFd::from_raw_fd(raw)
        } else {
            OwnedFd::from_raw_fd(raw)
        }
    };

    Ok(fd)
}

/// Attempt to become DRM master on a DRM device fd.
pub fn drm_set_master(fd: &OwnedFd) -> Result<(), DeviceError> {
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), DRM_IOCTL_SET_MASTER) };
    if ret < 0 {
        Err(DeviceError::DrmIoctl {
            fd: fd.as_raw_fd(),
            source: nix::Error::last(),
        })
    } else {
        Ok(())
    }
}

/// Drop DRM master status on a DRM device fd.
pub fn drm_drop_master(fd: &OwnedFd) -> Result<(), DeviceError> {
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), DRM_IOCTL_DROP_MASTER) };
    if ret < 0 {
        Err(DeviceError::DrmIoctl {
            fd: fd.as_raw_fd(),
            source: nix::Error::last(),
        })
    } else {
        Ok(())
    }
}

/// Check if a device is a DRM device by its major number.
pub fn is_drm_device(major: u32) -> bool {
    major == DRM_MAJOR
}

/// Try to resolve the real device path from sysfs.
fn resolve_devname(major: u32, minor: u32) -> Option<String> {
    let uevent_path = format!("/sys/dev/char/{}:{}/uevent", major, minor);
    let content = std::fs::read_to_string(uevent_path).ok()?;
    for line in content.lines() {
        if let Some(devname) = line.strip_prefix("DEVNAME=") {
            return Some(format!("/dev/{}", devname));
        }
    }
    None
}

// ---- ACL handover (uaccess equivalent) ----

/// Devices that get chowned to the active session's user.
/// We don't yet support per-seat tagging; everything is on seat0.
fn enumerate_session_devices(_seat_id: &str) -> Vec<(PathBuf, u32)> {
    let mut out = Vec::new();

    // DRM card / render nodes. Mode 0660; chgrp left alone, only chown.
    if let Ok(entries) = std::fs::read_dir("/dev/dri") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("card") || name.starts_with("renderD") {
                out.push((entry.path(), 0o660));
            }
        }
    }

    // Input devices (evdev, keyboards, mice).
    if let Ok(entries) = std::fs::read_dir("/dev/input") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("event") || name.starts_with("mouse") || name == "mice" {
                out.push((entry.path(), 0o660));
            }
        }
    }

    // Sound devices.
    if let Ok(entries) = std::fs::read_dir("/dev/snd") {
        for entry in entries.flatten() {
            out.push((entry.path(), 0o660));
        }
    }

    out
}

/// Chown the seat's devices to `uid` and ensure mode bits allow group access.
pub fn apply_session_acl(seat_id: &str, uid: u32) {
    for (path, mode) in enumerate_session_devices(seat_id) {
        if let Err(e) = chown_and_chmod(&path, Some(uid), None, mode) {
            tracing::debug!("ACL: {} chown({uid}) failed: {e}", path.display());
        }
    }
    tracing::info!("ACL: handed over seat={seat_id} devices to uid={uid}");
}

/// Revert ACLs back to root ownership.
pub fn revert_session_acl(seat_id: &str) {
    for (path, mode) in enumerate_session_devices(seat_id) {
        if let Err(e) = chown_and_chmod(&path, Some(0), None, mode) {
            tracing::debug!("ACL: {} revert chown(0) failed: {e}", path.display());
        }
    }
    tracing::debug!("ACL: reverted seat={seat_id} devices to root");
}

fn chown_and_chmod(
    path: &Path,
    uid: Option<u32>,
    gid: Option<u32>,
    mode: u32,
) -> std::io::Result<()> {
    use std::os::unix::fs::{chown, PermissionsExt};
    chown(path, uid, gid)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}
