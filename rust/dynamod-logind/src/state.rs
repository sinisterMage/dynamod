/// Central login state: sessions, seats, users, and inhibitors.
///
/// All mutable state lives behind a single `Arc<Mutex<LoginState>>` so
/// D-Bus method handlers can access it from any async task.
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::inhibitor::Inhibitor;

// ---------- identifiers ----------

pub type SessionId = String;
pub type SeatId = String;

// ---------- session ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Session is registered but not yet active.
    Online,
    /// Session is the active foreground session on its seat.
    Active,
    /// Session is being torn down.
    Closing,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Active => "active",
            Self::Closing => "closing",
        }
    }
}

#[derive(Debug)]
pub struct Session {
    pub id: SessionId,
    pub uid: u32,
    pub user_name: String,
    pub seat: Option<SeatId>,
    pub tty: Option<String>,
    pub display: Option<String>,
    pub session_type: String,
    pub class: String,
    pub desktop: String,
    pub state: SessionState,
    pub active: bool,
    pub leader_pid: u32,
    pub vtnr: u32,
    pub remote: bool,
    pub remote_host: String,
    pub remote_user: String,
    pub service: String,
    /// D-Bus unique name of the session controller (the compositor).
    pub controller: Option<String>,
    /// Open device fds keyed by (major, minor).
    pub devices: HashMap<(u32, u32), OwnedFd>,
    pub idle_hint: bool,
    pub idle_since_usec: u64,
    pub idle_since_monotonic: u64,
    pub locked_hint: bool,
    pub created_realtime_usec: u64,
    pub created_monotonic: Instant,
}

// ---------- seat ----------

#[derive(Debug)]
pub struct Seat {
    pub id: SeatId,
    pub sessions: Vec<SessionId>,
    pub active_session: Option<SessionId>,
    pub can_graphical: bool,
    pub can_tty: bool,
    pub idle_hint: bool,
    pub idle_since_usec: u64,
    pub idle_since_monotonic: u64,
}

// ---------- user ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserState {
    /// User has sessions but none are active.
    Online,
    /// User has at least one active session.
    Active,
    /// User has no sessions but lingers.
    Lingering,
    /// User sessions are being closed.
    Closing,
}

impl UserState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Active => "active",
            Self::Lingering => "lingering",
            Self::Closing => "closing",
        }
    }
}

#[derive(Debug)]
pub struct User {
    pub uid: u32,
    pub name: String,
    pub sessions: Vec<SessionId>,
    /// The primary graphical session for this user.
    pub display_session: Option<SessionId>,
    pub state: UserState,
    pub idle_hint: bool,
    pub idle_since_usec: u64,
    pub idle_since_monotonic: u64,
}

// ---------- aggregate state ----------

/// Global monotonic counter for session IDs.
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_session_id() -> SessionId {
    NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

/// Global monotonic counter for inhibitor cookie values.
static NEXT_INHIBITOR_COOKIE: AtomicU64 = AtomicU64::new(1);

pub fn next_inhibitor_cookie() -> u64 {
    NEXT_INHIBITOR_COOKIE.fetch_add(1, Ordering::Relaxed)
}

/// Returns the current wall-clock time as microseconds since the UNIX epoch.
pub fn now_realtime_usec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[derive(Debug)]
pub struct LoginState {
    pub sessions: HashMap<SessionId, Session>,
    pub seats: HashMap<SeatId, Seat>,
    pub users: HashMap<u32, User>,
    pub pid_to_session: HashMap<u32, SessionId>,
    pub inhibitors: Vec<Inhibitor>,
}

impl LoginState {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            seats: HashMap::new(),
            users: HashMap::new(),
            pid_to_session: HashMap::new(),
            inhibitors: Vec::new(),
        }
    }

    /// Create the default seat0.
    pub fn create_seat0(&mut self) {
        self.seats.insert(
            "seat0".to_string(),
            Seat {
                id: "seat0".to_string(),
                sessions: Vec::new(),
                active_session: None,
                can_graphical: true,
                can_tty: true,
                idle_hint: false,
                idle_since_usec: 0,
                idle_since_monotonic: 0,
            },
        );
    }

    /// Register a new session. Returns the session ID.
    pub fn create_session(
        &mut self,
        uid: u32,
        user_name: String,
        leader_pid: u32,
        service: String,
        session_type: String,
        class: String,
        desktop: String,
        seat_id: Option<String>,
        vtnr: u32,
        tty: Option<String>,
        display: Option<String>,
        remote: bool,
        remote_user: String,
        remote_host: String,
    ) -> SessionId {
        let id = next_session_id();
        let now_usec = now_realtime_usec();

        let is_first_on_seat = seat_id
            .as_ref()
            .and_then(|sid| self.seats.get(sid))
            .map_or(true, |s| s.active_session.is_none());

        let session = Session {
            id: id.clone(),
            uid,
            user_name: user_name.clone(),
            seat: seat_id.clone(),
            tty,
            display,
            session_type,
            class,
            desktop,
            state: if is_first_on_seat {
                SessionState::Active
            } else {
                SessionState::Online
            },
            active: is_first_on_seat,
            leader_pid,
            vtnr,
            remote,
            remote_host,
            remote_user,
            service,
            controller: None,
            devices: HashMap::new(),
            idle_hint: false,
            idle_since_usec: 0,
            idle_since_monotonic: 0,
            locked_hint: false,
            created_realtime_usec: now_usec,
            created_monotonic: Instant::now(),
        };

        // Track pid -> session
        self.pid_to_session.insert(leader_pid, id.clone());

        // Add to seat
        if let Some(ref sid) = seat_id {
            if let Some(seat) = self.seats.get_mut(sid) {
                seat.sessions.push(id.clone());
                if is_first_on_seat {
                    seat.active_session = Some(id.clone());
                }
            }
        }

        // Add to user (create user if needed)
        let user = self.users.entry(uid).or_insert_with(|| User {
            uid,
            name: user_name,
            sessions: Vec::new(),
            display_session: None,
            state: UserState::Online,
            idle_hint: false,
            idle_since_usec: 0,
            idle_since_monotonic: 0,
        });
        user.sessions.push(id.clone());
        if is_first_on_seat {
            user.state = UserState::Active;
            user.display_session = Some(id.clone());
        }

        self.sessions.insert(id.clone(), session);
        id
    }

    /// Remove a session and clean up all references.
    pub fn remove_session(&mut self, session_id: &str) {
        let Some(session) = self.sessions.remove(session_id) else {
            return;
        };

        // Remove from pid map
        self.pid_to_session.remove(&session.leader_pid);

        // Remove from seat
        if let Some(ref sid) = session.seat {
            if let Some(seat) = self.seats.get_mut(sid) {
                seat.sessions.retain(|s| s != session_id);
                if seat.active_session.as_deref() == Some(session_id) {
                    seat.active_session = seat.sessions.first().cloned();
                    // Activate the new foreground session
                    if let Some(ref new_active) = seat.active_session {
                        if let Some(s) = self.sessions.get_mut(new_active) {
                            s.active = true;
                            s.state = SessionState::Active;
                        }
                    }
                }
            }
        }

        // Remove from user
        if let Some(user) = self.users.get_mut(&session.uid) {
            user.sessions.retain(|s| s != session_id);
            if user.display_session.as_deref() == Some(session_id) {
                user.display_session = user.sessions.first().cloned();
            }
            if user.sessions.is_empty() {
                self.users.remove(&session.uid);
            } else {
                // Recalculate user state
                let any_active = user.sessions.iter().any(|sid| {
                    self.sessions
                        .get(sid)
                        .map_or(false, |s| s.state == SessionState::Active)
                });
                user.state = if any_active {
                    UserState::Active
                } else {
                    UserState::Online
                };
            }
        }
    }

    /// Look up a session by leader PID.
    pub fn session_for_pid(&self, pid: u32) -> Option<&str> {
        self.pid_to_session.get(&pid).map(|s| s.as_str())
    }

    /// Activate a session on its seat, deactivating the previously active one.
    pub fn activate_session(&mut self, session_id: &str) {
        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        let Some(seat_id) = session.seat.clone() else {
            return;
        };

        // Deactivate old active session on the same seat
        if let Some(seat) = self.seats.get(&seat_id) {
            if let Some(ref old_id) = seat.active_session {
                if old_id != session_id {
                    if let Some(old) = self.sessions.get_mut(old_id) {
                        old.active = false;
                        old.state = SessionState::Online;
                    }
                }
            }
        }

        // Activate new session
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.active = true;
            session.state = SessionState::Active;
        }

        if let Some(seat) = self.seats.get_mut(&seat_id) {
            seat.active_session = Some(session_id.to_string());
        }
    }

    /// Collect active "block" inhibitors for the given action.
    pub fn active_block_inhibitors(&self, what: &str) -> Vec<&Inhibitor> {
        self.inhibitors
            .iter()
            .filter(|i| i.what.contains(what) && i.mode == "block")
            .collect()
    }

    /// Collect active "delay" inhibitors for the given action.
    pub fn active_delay_inhibitors(&self, what: &str) -> Vec<&Inhibitor> {
        self.inhibitors
            .iter()
            .filter(|i| i.what.contains(what) && i.mode == "delay")
            .collect()
    }
}
