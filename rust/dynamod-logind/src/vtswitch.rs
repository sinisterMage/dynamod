/// VT (virtual terminal) switching support.
///
/// Uses Linux tty ioctls to switch between virtual terminals.
/// These are public kernel UAPI constants from <linux/vt.h>.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// VT ioctl numbers (from linux/vt.h, public kernel UAPI).
const VT_ACTIVATE: libc::Ioctl = 0x5606;
const VT_WAITACTIVE: libc::Ioctl = 0x5607;
const VT_GETSTATE: libc::Ioctl = 0x5603;

/// VT state struct returned by VT_GETSTATE.
#[repr(C)]
struct VtState {
    v_active: u16,
    v_signal: u16,
    v_state: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum VtError {
    #[error("failed to open /dev/tty0: {0}")]
    OpenTty(std::io::Error),
    #[error("VT ioctl failed: {0}")]
    Ioctl(nix::Error),
}

/// Open a handle to /dev/tty0 for VT control.
fn open_tty0() -> Result<OwnedFd, VtError> {
    let path = c"/dev/tty0";
    let fd = unsafe {
        libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY)
    };
    if fd < 0 {
        return Err(VtError::OpenTty(std::io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Switch to a specific VT number.
pub fn switch_to_vt(vt: u32) -> Result<(), VtError> {
    let tty = open_tty0()?;
    let ret = unsafe { libc::ioctl(tty.as_raw_fd(), VT_ACTIVATE, vt) };
    if ret < 0 {
        return Err(VtError::Ioctl(nix::Error::last()));
    }
    let ret = unsafe { libc::ioctl(tty.as_raw_fd(), VT_WAITACTIVE, vt) };
    if ret < 0 {
        return Err(VtError::Ioctl(nix::Error::last()));
    }
    Ok(())
}

/// Get the currently active VT number.
pub fn get_active_vt() -> Result<u32, VtError> {
    let tty = open_tty0()?;
    let mut state = VtState {
        v_active: 0,
        v_signal: 0,
        v_state: 0,
    };
    let ret = unsafe {
        libc::ioctl(
            tty.as_raw_fd(),
            VT_GETSTATE,
            &mut state as *mut VtState,
        )
    };
    if ret < 0 {
        return Err(VtError::Ioctl(nix::Error::last()));
    }
    Ok(state.v_active as u32)
}
