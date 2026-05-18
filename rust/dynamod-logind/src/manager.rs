/// org.freedesktop.login1.Manager D-Bus interface.
///
/// Clean-room implementation based on the freedesktop.org login1 interface
/// specification. No systemd source code was used.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, zvariant};

use crate::auth;
use crate::cgroup;
use crate::device;
use crate::inhibitor;
use crate::power::{self, ShutdownKind as PowerShutdownKind, SleepKind};
use crate::runtime_dir;
use crate::state::{self, LoginState, SeatId};

/// The Manager object lives at /org/freedesktop/login1 on the system bus.
pub struct Manager {
    pub state: Arc<Mutex<LoginState>>,
    pub object_server: Arc<Mutex<Option<zbus::ObjectServer>>>,
    /// System-bus connection, used to resolve caller credentials and to emit
    /// `PrepareForSleep` / `PrepareForShutdown` from background tasks.
    pub connection: zbus::Connection,
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

        // Look up user name + gid from uid via /etc/passwd.
        let (user_name, gid) = get_user_info(uid)
            .unwrap_or_else(|| (format!("uid{uid}"), uid));

        // If we have a pending cleanup for this user's runtime dir, cancel it
        // — they're logging back in before the grace period expired.
        if let Some(handle) = st.pending_user_cleanups.remove(&uid) {
            handle.abort();
        }

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

        // Read tmpfs size from config under the same lock acquisition as the
        // session was registered, then drop the lock before doing I/O.
        let runtime_size = st.config.read().await.runtime_directory_size.clone();
        let is_active = st.sessions.get(&id).map_or(false, |s| s.active);
        drop(st);

        // /run/user/$UID, cgroup placement, device ACL — all best-effort.
        if let Err(e) = runtime_dir::ensure(uid, gid, &runtime_size) {
            tracing::warn!("create_session: runtime_dir::ensure({uid}) failed: {e}");
        }
        cgroup::place_leader(uid, &id, pid);
        if is_active {
            if let Some(ref s) = seat {
                device::apply_session_acl(s, uid);
            }
        }

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
        let (uid, pid) = caller_credentials(&self.connection, &header).await;
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
        if !self.state.lock().await.sessions.contains_key(session_id) {
            return Err(fdo::Error::Failed(format!(
                "no session '{session_id}'"
            )));
        }
        finalize_session_removal(&self.state, session_id).await;
        Ok(())
    }

    /// Activate a session.
    async fn activate_session(&self, session_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        if !st.sessions.contains_key(session_id) {
            return Err(fdo::Error::Failed(format!(
                "no session '{session_id}'"
            )));
        }
        let prev_active = activate_with_acl_handover(&mut st, session_id);
        drop(st);
        run_acl_handover(prev_active);
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
        let prev_active = activate_with_acl_handover(&mut st, session_id);
        drop(st);
        run_acl_handover(prev_active);
        Ok(())
    }

    /// Terminate a session (kill all its processes).
    async fn terminate_session(&self, session_id: &str) -> fdo::Result<()> {
        let pid = {
            let st = self.state.lock().await;
            st.sessions
                .get(session_id)
                .map(|s| s.leader_pid)
                .ok_or_else(|| {
                    fdo::Error::Failed(format!("no session '{session_id}'"))
                })?
        };
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        finalize_session_removal(&self.state, session_id).await;
        Ok(())
    }

    /// Terminate a user's sessions.
    async fn terminate_user(&self, uid: u32) -> fdo::Result<()> {
        let session_ids: Vec<String> = {
            let st = self.state.lock().await;
            st.users
                .get(&uid)
                .map(|u| u.sessions.clone())
                .unwrap_or_default()
        };
        for sid in &session_ids {
            let pid_opt = self
                .state
                .lock()
                .await
                .sessions
                .get(sid)
                .map(|s| s.leader_pid);
            if let Some(pid) = pid_opt {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
        }
        for sid in session_ids {
            finalize_session_removal(&self.state, &sid).await;
        }
        Ok(())
    }

    /// Terminate a seat (close all sessions on it).
    async fn terminate_seat(&self, seat_id: &str) -> fdo::Result<()> {
        let session_ids = {
            let st = self.state.lock().await;
            let seat = st.seats.get(seat_id).ok_or_else(|| {
                fdo::Error::Failed(format!("no seat '{seat_id}'"))
            })?;
            seat.sessions.clone()
        };
        for sid in session_ids {
            let pid_opt = self
                .state
                .lock()
                .await
                .sessions
                .get(&sid)
                .map(|s| s.leader_pid);
            if let Some(pid) = pid_opt {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
            finalize_session_removal(&self.state, &sid).await;
        }
        Ok(())
    }

    // --- Power management ---

    /// Initiate system power-off.
    async fn power_off(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_shutdown(PowerShutdownKind::Poweroff).await
    }

    /// Initiate system reboot.
    async fn reboot(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_shutdown(PowerShutdownKind::Reboot).await
    }

    /// Initiate system halt.
    async fn halt(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_shutdown(PowerShutdownKind::Halt).await
    }

    /// Suspend the system.
    async fn suspend(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_sleep(SleepKind::Suspend).await
    }

    /// Hibernate the system.
    async fn hibernate(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_sleep(SleepKind::Hibernate).await
    }

    /// Hybrid suspend+hibernate.
    async fn hybrid_sleep(&self, interactive: bool) -> fdo::Result<()> {
        let _ = interactive;
        self.run_sleep(SleepKind::HybridSleep).await
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
        self.state.lock().await.config.read().await.n_auto_vts
    }

    #[zbus(property)]
    async fn kill_user_processes(&self) -> bool {
        self.state
            .lock()
            .await
            .config
            .read()
            .await
            .kill_user_processes
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
        self.state
            .lock()
            .await
            .config
            .read()
            .await
            .inhibit_delay_max
            .as_micros() as u64
    }

    #[zbus(property)]
    async fn user_stop_delay_u_sec(&self) -> u64 {
        self.state
            .lock()
            .await
            .config
            .read()
            .await
            .user_stop_delay
            .as_micros() as u64
    }
}

impl Manager {
    /// Construct a closure that emits PrepareForSleep on this Manager's
    /// D-Bus object path.
    fn prepare_for_sleep_emitter(&self) -> crate::acpi::PrepareEmitter {
        let conn = self.connection.clone();
        Arc::new(move |active: bool| -> zbus::Result<()> {
            let conn = conn.clone();
            tokio::spawn(async move {
                let _ = conn
                    .emit_signal(
                        None::<&str>,
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        "PrepareForSleep",
                        &(active,),
                    )
                    .await;
            });
            Ok(())
        })
    }

    fn prepare_for_shutdown_emitter(&self) -> crate::acpi::PrepareEmitter {
        let conn = self.connection.clone();
        Arc::new(move |active: bool| -> zbus::Result<()> {
            let conn = conn.clone();
            tokio::spawn(async move {
                let _ = conn
                    .emit_signal(
                        None::<&str>,
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        "PrepareForShutdown",
                        &(active,),
                    )
                    .await;
            });
            Ok(())
        })
    }

    async fn run_sleep(&self, kind: SleepKind) -> fdo::Result<()> {
        let emit = self.prepare_for_sleep_emitter();
        power::execute_sleep(Arc::clone(&self.state), move |on| emit(on), kind)
            .await
            .map_err(fdo::Error::Failed)
    }

    async fn run_shutdown(&self, kind: PowerShutdownKind) -> fdo::Result<()> {
        let emit = self.prepare_for_shutdown_emitter();
        power::execute_shutdown(Arc::clone(&self.state), move |on| emit(on), kind)
            .await
            .map_err(fdo::Error::Failed)
    }
}

// --- helpers ---

pub fn session_object_path(id: &str) -> String {
    format!(
        "/org/freedesktop/login1/session/{}",
        systemd_escape(id)
    )
}

pub fn seat_object_path(id: &str) -> String {
    format!(
        "/org/freedesktop/login1/seat/{}",
        // Seat IDs like "seat0" contain only safe chars so light escaping is fine
        escape_object_path_component(id)
    )
}

pub fn user_object_path(uid: u32) -> String {
    format!(
        "/org/freedesktop/login1/user/{}",
        systemd_escape(&uid.to_string())
    )
}

/// Escape a string using systemd's bus_label_escape scheme.
///
/// systemd encodes EVERY byte as `_XX` (two hex digits), producing
/// paths like `/org/freedesktop/login1/session/_31` for session "1".
/// This is what libelogind/libsystemd expects when it constructs
/// session object paths from session IDs.
pub fn systemd_escape(s: &str) -> String {
    let mut out = String::new();
    if s.is_empty() {
        return "_".to_string();
    }
    for b in s.bytes() {
        out.push_str(&format!("_{:02x}", b));
    }
    out
}

/// Light escaping for D-Bus object path components.
/// Only escapes non-alphanumeric characters (used for seat IDs etc.).
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

/// Resolve the caller's (uid, pid) via DBusProxy.GetConnectionCredentials.
/// Returns (0, 0) if anything goes wrong — keeps backward compat with the old
/// stub behavior and never breaks Inhibit().
async fn caller_credentials(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
) -> (u32, u32) {
    let Some(sender) = header.sender() else {
        return (0, 0);
    };
    let proxy = match zbus::fdo::DBusProxy::new(conn).await {
        Ok(p) => p,
        Err(_) => return (0, 0),
    };
    let bus_name = zbus::names::BusName::Unique(sender.to_owned());
    let creds = match proxy.get_connection_credentials(bus_name).await {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };
    let uid = creds.unix_user_id().unwrap_or(0);
    let pid = creds.process_id().unwrap_or(0);
    (uid, pid)
}

/// Look up (username, gid) for a uid via /etc/passwd.
fn get_user_info(uid: u32) -> Option<(String, u32)> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| (u.name, u.gid.as_raw()))
}

/// Information captured before activating a new session, used to drive the
/// device-ACL handover after the state lock is released.
#[derive(Debug, Default)]
struct AclHandover {
    seat: Option<SeatId>,
    old_uid: Option<u32>,
    new_uid: Option<u32>,
}

fn activate_with_acl_handover(st: &mut LoginState, session_id: &str) -> AclHandover {
    let (seat_id, new_uid) = match st.sessions.get(session_id) {
        Some(s) => (s.seat.clone(), s.uid),
        None => return AclHandover::default(),
    };
    let old_uid = seat_id
        .as_ref()
        .and_then(|sid| st.seats.get(sid))
        .and_then(|s| s.active_session.as_ref())
        .filter(|id| *id != session_id)
        .and_then(|id| st.sessions.get(id))
        .map(|s| s.uid);

    st.activate_session(session_id);

    AclHandover {
        seat: seat_id,
        old_uid,
        new_uid: Some(new_uid),
    }
}

fn run_acl_handover(handover: AclHandover) {
    let Some(seat) = handover.seat else { return };
    if let Some(old) = handover.old_uid {
        if handover.new_uid != Some(old) {
            device::revert_session_acl(&seat);
        }
    }
    if let Some(new) = handover.new_uid {
        device::apply_session_acl(&seat, new);
    }
}

/// Remove a session and trigger the runtime-dir/cgroup teardown chain.
/// Schedules `/run/user/$UID` removal after `UserStopDelaySec` if this was
/// the user's last session.
async fn finalize_session_removal(
    state: &Arc<Mutex<LoginState>>,
    session_id: &str,
) {
    let mut st = state.lock().await;
    let Some(session) = st.sessions.get(session_id) else {
        return;
    };
    let uid = session.uid;
    let was_active = session.active;
    let seat = session.seat.clone();

    st.remove_session(session_id);

    // Best-effort cgroup scope cleanup. The svmgr's own cgroups are untouched.
    cgroup::release_scope(uid, session_id);

    let user_gone = !st.users.contains_key(&uid);
    let user_stop_delay = st.config.read().await.user_stop_delay;

    if user_gone {
        let state_clone = Arc::clone(state);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(user_stop_delay).await;
            let still_gone = {
                let st = state_clone.lock().await;
                !st.users.contains_key(&uid)
            };
            if still_gone {
                runtime_dir::release(uid);
                cgroup::release_user_slice(uid);
                state_clone.lock().await.pending_user_cleanups.remove(&uid);
                tracing::info!("user {uid} runtime dir + cgroup slice released");
            }
        });
        st.pending_user_cleanups.insert(uid, handle);
    }

    let new_active_uid = seat
        .as_ref()
        .and_then(|sid| st.seats.get(sid))
        .and_then(|s| s.active_session.clone())
        .and_then(|sid| st.sessions.get(&sid))
        .map(|s| s.uid);

    drop(st);

    if was_active {
        if let Some(s) = &seat {
            if Some(uid) != new_active_uid {
                device::revert_session_acl(s);
            }
            if let Some(new) = new_active_uid {
                if new != uid {
                    device::apply_session_acl(s, new);
                }
            }
        }
    }
}


// Re-export for use in other modules

// Convenience aliases
pub fn session_path(id: &str) -> String { session_object_path(id) }
pub fn seat_path(id: &str) -> String { seat_object_path(id) }
pub fn user_path(uid: u32) -> String { user_object_path(uid) }
