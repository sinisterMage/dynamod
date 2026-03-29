/// org.freedesktop.login1.Session D-Bus interface.
///
/// Each session gets its own D-Bus object at
/// /org/freedesktop/login1/session/<id>.
///
/// Clean-room implementation based on the freedesktop.org login1 specification.
use std::os::fd::OwnedFd;
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, zvariant};

use crate::device;
use crate::manager;
use crate::state::{LoginState, SessionState};

pub struct SessionInterface {
    pub session_id: String,
    pub state: Arc<Mutex<LoginState>>,
}

#[interface(name = "org.freedesktop.login1.Session")]
impl SessionInterface {
    /// Allow a D-Bus client (usually a Wayland compositor) to become the
    /// controller of this session. Only one controller is allowed at a time.
    async fn take_control(
        &self,
        force: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;

        if session.controller.is_some() && !force {
            return Err(fdo::Error::Failed(
                "session already has a controller".to_string(),
            ));
        }

        // Record the caller's D-Bus unique name as the controller
        let sender = header
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_default();
        session.controller = Some(sender);

        tracing::info!(
            session = %self.session_id,
            "session controller taken"
        );
        Ok(())
    }

    /// Release session control. All taken devices are released.
    async fn release_control(&self) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;

        session.controller = None;
        // Release all devices
        session.devices.clear();

        tracing::info!(
            session = %self.session_id,
            "session controller released, all devices closed"
        );
        Ok(())
    }

    /// Open a device and return its fd. This is the critical method for
    /// Wayland compositors to access DRM and input devices without root.
    ///
    /// Returns: (fd, inactive)
    /// - fd: the opened device file descriptor
    /// - inactive: whether the session is currently inactive (device paused)
    async fn take_device(
        &self,
        major: u32,
        minor: u32,
    ) -> fdo::Result<(zvariant::OwnedFd, bool)> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;

        // Only the controller may take devices
        if session.controller.is_none() {
            return Err(fdo::Error::Failed(
                "caller is not the session controller".to_string(),
            ));
        }

        // Check if already taken
        if session.devices.contains_key(&(major, minor)) {
            return Err(fdo::Error::Failed(format!(
                "device {major}:{minor} already taken"
            )));
        }

        // Open the device
        let fd = device::open_device(major, minor)
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        // For DRM devices, become master if we're the active session
        let inactive = !session.active;
        if device::is_drm_device(major) && session.active {
            if let Err(e) = device::drm_set_master(&fd) {
                tracing::warn!(
                    session = %self.session_id,
                    "failed to set DRM master: {e}"
                );
                // Non-fatal: some drivers don't support it
            }
        }

        tracing::info!(
            session = %self.session_id,
            major, minor, inactive,
            "device taken"
        );

        // We need to dup the fd: one for our tracking, one for the caller
        let caller_fd = dup_fd(&fd)?;
        session.devices.insert((major, minor), fd);

        Ok((zvariant::OwnedFd::from(caller_fd), inactive))
    }

    /// Release a previously taken device.
    async fn release_device(
        &self,
        major: u32,
        minor: u32,
    ) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;

        if session.devices.remove(&(major, minor)).is_some() {
            tracing::info!(
                session = %self.session_id,
                major, minor,
                "device released"
            );
            Ok(())
        } else {
            Err(fdo::Error::Failed(format!(
                "device {major}:{minor} not taken"
            )))
        }
    }

    /// Acknowledge a device pause (sent during VT/session switch).
    async fn pause_device_complete(
        &self,
        major: u32,
        minor: u32,
    ) -> fdo::Result<()> {
        tracing::debug!(
            session = %self.session_id,
            major, minor,
            "pause device acknowledged"
        );
        Ok(())
    }

    /// Change the session type (e.g., from "tty" to "wayland").
    async fn set_type(&self, session_type: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;

        match session_type {
            "tty" | "x11" | "wayland" | "mir" | "unspecified" => {
                session.session_type = session_type.to_string();
                tracing::info!(
                    session = %self.session_id,
                    session_type,
                    "session type changed"
                );
                Ok(())
            }
            _ => Err(fdo::Error::InvalidArgs(format!(
                "unknown session type '{session_type}'"
            ))),
        }
    }

    /// Bring this session to the foreground.
    async fn activate(&self) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        st.activate_session(&self.session_id);
        Ok(())
    }

    /// Set the idle hint on this session.
    async fn set_idle_hint(&self, idle: bool) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get_mut(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        session.idle_hint = idle;
        if idle {
            session.idle_since_usec = crate::state::now_realtime_usec();
        } else {
            session.idle_since_usec = 0;
        }
        Ok(())
    }

    /// Terminate this session (kill its processes).
    async fn terminate(&self) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        let pid = session.leader_pid;
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        st.remove_session(&self.session_id);
        Ok(())
    }

    /// Request screen lock.
    #[zbus(signal)]
    async fn lock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Request screen unlock.
    #[zbus(signal)]
    async fn unlock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Signal: a device is being paused (session switching).
    /// type: "pause", "force", "gone"
    #[zbus(signal)]
    async fn pause_device(
        emitter: &SignalEmitter<'_>,
        major: u32,
        minor: u32,
        pause_type: &str,
    ) -> zbus::Result<()>;

    /// Signal: a paused device is being resumed.
    #[zbus(signal)]
    async fn resume_device(
        emitter: &SignalEmitter<'_>,
        major: u32,
        minor: u32,
        fd: zvariant::OwnedFd,
    ) -> zbus::Result<()>;

    // --- Properties ---

    #[zbus(property)]
    async fn id(&self) -> String {
        self.session_id.clone()
    }

    #[zbus(property)]
    async fn user(&self) -> fdo::Result<(u32, zvariant::OwnedObjectPath)> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        let path = zvariant::OwnedObjectPath::try_from(manager::user_path(session.uid))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok((session.uid, path))
    }

    #[zbus(property)]
    async fn name(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.user_name.clone())
    }

    #[zbus(property)]
    async fn timestamp(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.created_realtime_usec)
    }

    #[zbus(property)]
    async fn timestamp_monotonic(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.created_monotonic.elapsed().as_micros() as u64)
    }

    #[zbus(property, name = "VTNr")]
    async fn vtnr(&self) -> fdo::Result<u32> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.vtnr)
    }

    #[zbus(property)]
    async fn seat(&self) -> fdo::Result<(String, zvariant::OwnedObjectPath)> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        let seat_id = session.seat.clone().unwrap_or_default();
        let path = if seat_id.is_empty() {
            zvariant::OwnedObjectPath::try_from("/")
                .map_err(|e| fdo::Error::Failed(e.to_string()))?
        } else {
            zvariant::OwnedObjectPath::try_from(manager::seat_path(&seat_id))
                .map_err(|e| fdo::Error::Failed(e.to_string()))?
        };
        Ok((seat_id, path))
    }

    #[zbus(property, name = "TTY")]
    async fn tty(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.tty.clone().unwrap_or_default())
    }

    #[zbus(property)]
    async fn display(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.display.clone().unwrap_or_default())
    }

    #[zbus(property)]
    async fn remote(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.remote)
    }

    #[zbus(property)]
    async fn remote_host(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.remote_host.clone())
    }

    #[zbus(property)]
    async fn remote_user(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.remote_user.clone())
    }

    #[zbus(property)]
    async fn service(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.service.clone())
    }

    #[zbus(property)]
    async fn desktop(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.desktop.clone())
    }

    #[zbus(property)]
    async fn scope(&self) -> fdo::Result<String> {
        Ok(format!("session-{}.scope", self.session_id))
    }

    #[zbus(property)]
    async fn leader(&self) -> fdo::Result<u32> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.leader_pid)
    }

    #[zbus(property, name = "Type")]
    async fn session_type(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.session_type.clone())
    }

    #[zbus(property)]
    async fn class(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.class.clone())
    }

    #[zbus(property)]
    async fn active(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.active)
    }

    #[zbus(property)]
    async fn state(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.state.as_str().to_string())
    }

    #[zbus(property)]
    async fn idle_hint(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.idle_hint)
    }

    #[zbus(property)]
    async fn idle_since_hint(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.idle_since_usec)
    }

    #[zbus(property)]
    async fn idle_since_hint_monotonic(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.idle_since_monotonic)
    }

    #[zbus(property)]
    async fn locked_hint(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let session = st.sessions.get(&self.session_id).ok_or_else(|| {
            fdo::Error::Failed("session not found".to_string())
        })?;
        Ok(session.locked_hint)
    }
}

/// Duplicate a file descriptor.
fn dup_fd(fd: &OwnedFd) -> fdo::Result<OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let new_fd = unsafe { libc::dup(fd.as_raw_fd()) };
    if new_fd < 0 {
        return Err(fdo::Error::Failed(format!(
            "dup failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // Set CLOEXEC
    unsafe {
        libc::fcntl(new_fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    Ok(unsafe { OwnedFd::from_raw_fd(new_fd) })
}
