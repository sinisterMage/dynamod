/// org.freedesktop.login1.Manager D-Bus interface.
///
/// Clean-room implementation based on the freedesktop.org login1 interface
/// specification. No systemd source code was used.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, zvariant};

use crate::auth;
use crate::inhibitor;
use crate::state::{self, LoginState, SeatId, SessionId};
use crate::svmgr_client;

/// The Manager object lives at /org/freedesktop/login1 on the system bus.
pub struct Manager {
    pub state: Arc<Mutex<LoginState>>,
    pub object_server: Arc<Mutex<Option<zbus::ObjectServer>>>,
}

#[interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    /// Create a new login session.
    ///
    /// This is normally called by PAM (pam_systemd) during login.
    /// Returns: (session_id, object_path, runtime_path, fd, uid, seat_id, vtnr, existing)
    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        uid: u32,
        pid: u32,
        service: &str,
        session_type: &str,
        class: &str,
        desktop: &str,
        seat_id: &str,
        vtnr: u32,
        tty: &str,
        display: &str,
        remote: bool,
        remote_user: &str,
        remote_host: &str,
        #[zbus(header)] _header: zbus::message::Header<'_>,
    ) -> fdo::Result<(
        String,                               // session_id
        zvariant::OwnedObjectPath,            // object_path
        String,                               // runtime_path
        String,                               // seat_id
        u32,                                  // vtnr
        bool,                                 // existing
    )> {
        let mut st = self.state.lock().await;

        // Determine seat: use provided or default to "seat0"
        let seat = if seat_id.is_empty() {
            if st.seats.contains_key("seat0") {
                Some("seat0".to_string())
            } else {
                None
            }
        } else if st.seats.contains_key(seat_id) {
            Some(seat_id.to_string())
        } else {
            return Err(fdo::Error::InvalidArgs(format!(
                "seat '{seat_id}' does not exist"
            )));
        };

        // Look up user name from uid
        let user_name = get_username(uid).unwrap_or_else(|| format!("uid{uid}"));

        let id = st.create_session(
            uid,
            user_name,
            pid,
            service.to_string(),
            if session_type.is_empty() {
                "tty".to_string()
            } else {
                session_type.to_string()
            },
            if class.is_empty() {
                "user".to_string()
            } else {
                class.to_string()
            },
            desktop.to_string(),
            seat.clone(),
            vtnr,
            if tty.is_empty() {
                None
            } else {
                Some(tty.to_string())
            },
            if display.is_empty() {
                None
            } else {
                Some(display.to_string())
            },
            remote,
            remote_user.to_string(),
            remote_host.to_string(),
        );

        let obj_path = session_object_path(&id);
        let runtime_path = format!("/run/user/{uid}");

        // Register session D-Bus object
        if let Some(ref server) = *self.object_server.lock().await {
            let session_iface = crate::session::SessionInterface {
                session_id: id.clone(),
                state: Arc::clone(&self.state),
            };
            let path = obj_path.clone();
            let _ = server
                .at(path.as_str(), session_iface)
                .await;
        }

        // Register user D-Bus object if this is the first session
        if let Some(ref server) = *self.object_server.lock().await {
            let user_path = user_object_path(uid);
            let user_iface = crate::user::UserInterface {
                uid,
                state: Arc::clone(&self.state),
            };
            // at() returns false if already registered, which is fine
            let _ = server
                .at(user_path.as_str(), user_iface)
                .await;
        }

        Ok((
            id,
            zvariant::OwnedObjectPath::try_from(obj_path)
                .map_err(|e| fdo::Error::Failed(e.to_string()))?,
            runtime_path,
            seat.unwrap_or_default(),
            vtnr,
            false,
        ))
    }

    /// Get the D-Bus object path for a session by ID.
    async fn get_session(
        &self,
        session_id: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let st = self.state.lock().await;
        if st.sessions.contains_key(session_id) {
            zvariant::OwnedObjectPath::try_from(session_object_path(session_id))
                .map_err(|e| fdo::Error::Failed(e.to_string()))
        } else {
            Err(fdo::Error::Failed(format!(
                "no session '{session_id}'"
            )))
        }
    }

    /// Get the D-Bus object path for a session by the leader PID.
    async fn get_session_by_pid(
        &self,
        pid: u32,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let st = self.state.lock().await;
        match st.session_for_pid(pid) {
            Some(id) => {
                zvariant::OwnedObjectPath::try_from(session_object_path(id))
                    .map_err(|e| fdo::Error::Failed(e.to_string()))
            }
            None => Err(fdo::Error::Failed(format!(
                "no session for PID {pid}"
            ))),
        }
    }

    /// Get the D-Bus object path for a seat by ID.
    async fn get_seat(
        &self,
        seat_id: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let st = self.state.lock().await;
        if st.seats.contains_key(seat_id) {
            zvariant::OwnedObjectPath::try_from(seat_object_path(seat_id))
                .map_err(|e| fdo::Error::Failed(e.to_string()))
        } else {
            Err(fdo::Error::Failed(format!(
                "no seat '{seat_id}'"
            )))
        }
    }

    /// Get the D-Bus object path for a user by UID.
    async fn get_user(
        &self,
        uid: u32,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let st = self.state.lock().await;
        if st.users.contains_key(&uid) {
            zvariant::OwnedObjectPath::try_from(user_object_path(uid))
                .map_err(|e| fdo::Error::Failed(e.to_string()))
        } else {
            Err(fdo::Error::Failed(format!("no user with UID {uid}")))
        }
    }

    /// List all active sessions.
    /// Returns: array of (session_id, uid, user_name, seat_id, object_path)
    async fn list_sessions(
        &self,
    ) -> fdo::Result<Vec<(String, u32, String, String, zvariant::OwnedObjectPath)>> {
        let st = self.state.lock().await;
        let mut result = Vec::new();
        for session in st.sessions.values() {
            let path = zvariant::OwnedObjectPath::try_from(session_object_path(&session.id))
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            result.push((
                session.id.clone(),
                session.uid,
                session.user_name.clone(),
                session.seat.clone().unwrap_or_default(),
                path,
            ));
        }
        Ok(result)
    }

    /// List all seats.
    /// Returns: array of (seat_id, object_path)
    async fn list_seats(
        &self,
    ) -> fdo::Result<Vec<(String, zvariant::OwnedObjectPath)>> {
        let st = self.state.lock().await;
        let mut result = Vec::new();
        for seat in st.seats.values() {
            let path = zvariant::OwnedObjectPath::try_from(seat_object_path(&seat.id))
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            result.push((seat.id.clone(), path));
        }
        Ok(result)
    }

    /// List all logged-in users.
    /// Returns: array of (uid, user_name, object_path)
    async fn list_users(
        &self,
    ) -> fdo::Result<Vec<(u32, String, zvariant::OwnedObjectPath)>> {
        let st = self.state.lock().await;
        let mut result = Vec::new();
        for user in st.users.values() {
            let path = zvariant::OwnedObjectPath::try_from(user_object_path(user.uid))
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            result.push((user.uid, user.name.clone(), path));
        }
        Ok(result)
    }

    /// Create an inhibitor lock.
    ///
    /// Returns a file descriptor. The inhibitor is released when the fd is closed.
    async fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zvariant::OwnedFd> {
        let uid = get_caller_uid(&header);
        let pid = get_caller_pid(&header);
        let cookie = state::next_inhibitor_cookie();

        let (inh, client_fd) = inhibitor::create_inhibitor(
            what.to_string(),
            who.to_string(),
            why.to_string(),
            mode.to_string(),
            uid,
            pid,
            cookie,
        )
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        self.state.lock().await.inhibitors.push(inh);

        Ok(zvariant::OwnedFd::from(client_fd))
    }

    /// List active inhibitors.
    /// Returns: array of (what, who, why, mode, uid, pid)
    async fn list_inhibitors(
        &self,
    ) -> fdo::Result<Vec<(String, String, String, String, u32, u32)>> {
        let mut st = self.state.lock().await;
        // Garbage-collect released inhibitors
        st.inhibitors.retain(|i| !i.is_released());

        Ok(st
            .inhibitors
            .iter()
            .map(|i| {
                (
                    i.what.clone(),
                    i.who.clone(),
                    i.why.clone(),
                    i.mode.clone(),
                    i.uid,
                    i.pid,
                )
            })
            .collect())
    }

    /// Release a session.
    async fn release_session(&self, session_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        if st.sessions.contains_key(session_id) {
            st.remove_session(session_id);
            Ok(())
        } else {
            Err(fdo::Error::Failed(format!(
                "no session '{session_id}'"
            )))
        }
    }

    /// Activate a session.
    async fn activate_session(&self, session_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        if !st.sessions.contains_key(session_id) {
            return Err(fdo::Error::Failed(format!(
                "no session '{session_id}'"
            )));
        }
        st.activate_session(session_id);
        Ok(())
    }

    /// Activate a session on a specific seat.
    async fn activate_session_on_seat(
        &self,
        session_id: &str,
        seat_id: &str,
    ) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get(session_id).ok_or_else(|| {
            fdo::Error::Failed(format!("no session '{session_id}'"))
        })?;
        if session.seat.as_deref() != Some(seat_id) {
            return Err(fdo::Error::Failed(format!(
                "session '{session_id}' is not on seat '{seat_id}'"
            )));
        }
        st.activate_session(session_id);
        Ok(())
    }

    /// Terminate a session (kill all its processes).
    async fn terminate_session(&self, session_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session = st.sessions.get(session_id).ok_or_else(|| {
            fdo::Error::Failed(format!("no session '{session_id}'"))
        })?;
        // Kill the session leader
        let pid = session.leader_pid;
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        st.remove_session(session_id);
        Ok(())
    }

    /// Terminate a user's sessions.
    async fn terminate_user(&self, uid: u32) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session_ids: Vec<String> = st
            .users
            .get(&uid)
            .map(|u| u.sessions.clone())
            .unwrap_or_default();
        for sid in &session_ids {
            if let Some(session) = st.sessions.get(sid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(session.leader_pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
        }
        for sid in session_ids {
            st.remove_session(&sid);
        }
        Ok(())
    }

    /// Terminate a seat (close all sessions on it).
    async fn terminate_seat(&self, seat_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let seat = st.seats.get(seat_id).ok_or_else(|| {
            fdo::Error::Failed(format!("no seat '{seat_id}'"))
        })?;
        let session_ids = seat.sessions.clone();
        for sid in session_ids {
            if let Some(session) = st.sessions.get(&sid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(session.leader_pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
            st.remove_session(&sid);
        }
        Ok(())
    }

    // --- Power management ---

    /// Initiate system power-off.
    async fn power_off(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_shutdown(dynamod_common::protocol::ShutdownKind::Poweroff).await
    }

    /// Initiate system reboot.
    async fn reboot(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_shutdown(dynamod_common::protocol::ShutdownKind::Reboot).await
    }

    /// Initiate system halt.
    async fn halt(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_shutdown(dynamod_common::protocol::ShutdownKind::Halt).await
    }

    /// Suspend the system.
    async fn suspend(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_power_state("mem").await
    }

    /// Hibernate the system.
    async fn hibernate(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_power_state("disk").await
    }

    /// Hybrid suspend+hibernate.
    async fn hybrid_sleep(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        do_power_state("disk").await
    }

    /// Check if power-off is possible.
    async fn can_power_off(&self) -> fdo::Result<String> {
        Ok(auth::check_power_action(0, "poweroff").to_string())
    }

    /// Check if reboot is possible.
    async fn can_reboot(&self) -> fdo::Result<String> {
        Ok(auth::check_power_action(0, "reboot").to_string())
    }

    /// Check if halt is possible.
    async fn can_halt(&self) -> fdo::Result<String> {
        Ok(auth::check_power_action(0, "halt").to_string())
    }

    /// Check if suspend is possible.
    async fn can_suspend(&self) -> fdo::Result<String> {
        let can = std::fs::read_to_string("/sys/power/state")
            .map(|s| s.contains("mem"))
            .unwrap_or(false);
        Ok(if can { "yes" } else { "na" }.to_string())
    }

    /// Check if hibernate is possible.
    async fn can_hibernate(&self) -> fdo::Result<String> {
        let can = std::fs::read_to_string("/sys/power/state")
            .map(|s| s.contains("disk"))
            .unwrap_or(false);
        Ok(if can { "yes" } else { "na" }.to_string())
    }

    /// Check if hybrid sleep is possible.
    async fn can_hybrid_sleep(&self) -> fdo::Result<String> {
        self.can_hibernate().await
    }

    // --- Signals ---

    /// Emitted when a new session is created.
    #[zbus(signal)]
    async fn session_new(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when a session is removed.
    #[zbus(signal)]
    async fn session_removed(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when a new seat appears.
    #[zbus(signal)]
    async fn seat_new(
        emitter: &SignalEmitter<'_>,
        seat_id: &str,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when a seat is removed.
    #[zbus(signal)]
    async fn seat_removed(
        emitter: &SignalEmitter<'_>,
        seat_id: &str,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when a new user logs in.
    #[zbus(signal)]
    async fn user_new(
        emitter: &SignalEmitter<'_>,
        uid: u32,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when a user logs out completely.
    #[zbus(signal)]
    async fn user_removed(
        emitter: &SignalEmitter<'_>,
        uid: u32,
        object_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted before shutdown/reboot.
    #[zbus(signal)]
    async fn prepare_for_shutdown(
        emitter: &SignalEmitter<'_>,
        active: bool,
    ) -> zbus::Result<()>;

    /// Emitted before sleep (suspend/hibernate).
    #[zbus(signal)]
    async fn prepare_for_sleep(
        emitter: &SignalEmitter<'_>,
        active: bool,
    ) -> zbus::Result<()>;

    // --- Properties ---

    #[zbus(property)]
    async fn idle_hint(&self) -> bool {
        let st = self.state.lock().await;
        st.sessions.values().all(|s| s.idle_hint)
    }

    #[zbus(property)]
    async fn idle_since_hint(&self) -> u64 {
        0
    }

    #[zbus(property)]
    async fn idle_since_hint_monotonic(&self) -> u64 {
        0
    }

    #[zbus(property)]
    async fn block_inhibited(&self) -> String {
        let st = self.state.lock().await;
        let mut whats: Vec<&str> = Vec::new();
        for inh in &st.inhibitors {
            if inh.mode == "block" {
                for w in inh.what.split(':') {
                    if !whats.contains(&w) {
                        whats.push(w);
                    }
                }
            }
        }
        whats.join(":")
    }

    #[zbus(property)]
    async fn delay_inhibited(&self) -> String {
        let st = self.state.lock().await;
        let mut whats: Vec<&str> = Vec::new();
        for inh in &st.inhibitors {
            if inh.mode == "delay" {
                for w in inh.what.split(':') {
                    if !whats.contains(&w) {
                        whats.push(w);
                    }
                }
            }
        }
        whats.join(":")
    }

    #[zbus(property, name = "NAutoVTs")]
    async fn n_auto_vts(&self) -> u32 {
        6
    }

    #[zbus(property)]
    async fn kill_user_processes(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn docked(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn lid_closed(&self) -> bool {
        // Check ACPI lid state
        std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
            .map(|s| s.contains("closed"))
            .unwrap_or(false)
    }

    #[zbus(property)]
    async fn on_external_power(&self) -> bool {
        // Check AC power
        std::fs::read_to_string("/sys/class/power_supply/AC/online")
            .map(|s| s.trim() == "1")
            .unwrap_or(true)
    }

    #[zbus(property)]
    async fn inhibit_delay_max_u_sec(&self) -> u64 {
        5_000_000 // 5 seconds
    }

    #[zbus(property)]
    async fn user_stop_delay_u_sec(&self) -> u64 {
        10_000_000 // 10 seconds
    }
}

// --- helpers ---

pub fn session_object_path(id: &str) -> String {
    format!(
        "/org/freedesktop/login1/session/{}",
        escape_object_path_component(id)
    )
}

pub fn seat_object_path(id: &str) -> String {
    format!(
        "/org/freedesktop/login1/seat/{}",
        escape_object_path_component(id)
    )
}

pub fn user_object_path(uid: u32) -> String {
    format!("/org/freedesktop/login1/user/_{uid}")
}

/// Escape a string for use in D-Bus object paths.
/// Replace anything that's not [A-Za-z0-9] with _XX hex encoding.
pub fn escape_object_path_component(s: &str) -> String {
    let mut out = String::new();
    if s.is_empty() {
        return "_".to_string();
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("_{:02x}", b));
        }
    }
    out
}

async fn do_shutdown(kind: dynamod_common::protocol::ShutdownKind) -> fdo::Result<()> {
    tokio::task::spawn_blocking(move || svmgr_client::request_shutdown(kind))
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?
        .map_err(|e| fdo::Error::Failed(e.to_string()))
}

async fn do_power_state(state: &str) -> fdo::Result<()> {
    let state = state.to_string();
    tokio::task::spawn_blocking(move || {
        std::fs::write("/sys/power/state", &state)
    })
    .await
    .map_err(|e| fdo::Error::Failed(e.to_string()))?
    .map_err(|e| fdo::Error::Failed(e.to_string()))
}

fn get_caller_uid(_header: &zbus::message::Header<'_>) -> u32 {
    // In a full implementation, we would use the D-Bus credentials
    // to get the caller's UID. For now, return 0 (root).
    0
}

fn get_caller_pid(_header: &zbus::message::Header<'_>) -> u32 {
    0
}

fn get_username(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

// Re-export for use in other modules
pub use escape_object_path_component as escape_path;

// Convenience aliases
pub fn session_path(id: &str) -> String { session_object_path(id) }
pub fn seat_path(id: &str) -> String { seat_object_path(id) }
pub fn user_path(uid: u32) -> String { user_object_path(uid) }
