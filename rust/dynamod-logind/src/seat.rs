/// org.freedesktop.login1.Seat D-Bus interface.
///
/// Each seat gets its own D-Bus object at
/// /org/freedesktop/login1/seat/<id>.
///
/// Clean-room implementation based on the freedesktop.org login1 specification.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::{fdo, interface, zvariant};

use crate::manager;
use crate::state::LoginState;
use crate::vtswitch;

pub struct SeatInterface {
    pub seat_id: String,
    pub state: Arc<Mutex<LoginState>>,
}

#[interface(name = "org.freedesktop.login1.Seat")]
impl SeatInterface {
    /// Switch to the session with the given ID.
    async fn activate_session(&self, session_id: &str) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        if !seat.sessions.contains(&session_id.to_string()) {
            return Err(fdo::Error::Failed(format!(
                "session '{session_id}' is not on seat '{}'",
                self.seat_id
            )));
        }
        st.activate_session(session_id);
        Ok(())
    }

    /// Switch to a specific VT number.
    async fn switch_to(&self, vtnr: u32) -> fdo::Result<()> {
        tokio::task::spawn_blocking(move || vtswitch::switch_to_vt(vtnr))
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Switch to the next VT.
    async fn switch_to_next(&self) -> fdo::Result<()> {
        let current = tokio::task::spawn_blocking(vtswitch::get_active_vt)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let next = current + 1;
        tokio::task::spawn_blocking(move || vtswitch::switch_to_vt(next))
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Switch to the previous VT.
    async fn switch_to_previous(&self) -> fdo::Result<()> {
        let current = tokio::task::spawn_blocking(vtswitch::get_active_vt)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        if current > 1 {
            let prev = current - 1;
            tokio::task::spawn_blocking(move || vtswitch::switch_to_vt(prev))
                .await
                .map_err(|e| fdo::Error::Failed(e.to_string()))?
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        }
        Ok(())
    }

    /// Terminate all sessions on this seat.
    async fn terminate(&self) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
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

    // --- Properties ---

    #[zbus(property)]
    async fn id(&self) -> String {
        self.seat_id.clone()
    }

    #[zbus(property)]
    async fn active_session(
        &self,
    ) -> fdo::Result<(String, zvariant::OwnedObjectPath)> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        match &seat.active_session {
            Some(sid) => {
                let path =
                    zvariant::OwnedObjectPath::try_from(manager::session_path(sid))
                        .map_err(|e| fdo::Error::Failed(e.to_string()))?;
                Ok((sid.clone(), path))
            }
            None => {
                let path = zvariant::OwnedObjectPath::try_from("/")
                    .map_err(|e| fdo::Error::Failed(e.to_string()))?;
                Ok((String::new(), path))
            }
        }
    }

    #[zbus(property)]
    async fn sessions(
        &self,
    ) -> fdo::Result<Vec<(String, zvariant::OwnedObjectPath)>> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        let mut result = Vec::new();
        for sid in &seat.sessions {
            let path =
                zvariant::OwnedObjectPath::try_from(manager::session_path(sid))
                    .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            result.push((sid.clone(), path));
        }
        Ok(result)
    }

    #[zbus(property)]
    async fn can_graphical(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        Ok(seat.can_graphical)
    }

    #[zbus(property, name = "CanTTY")]
    async fn can_tty(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        Ok(seat.can_tty)
    }

    #[zbus(property)]
    async fn idle_hint(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        Ok(seat.idle_hint)
    }

    #[zbus(property)]
    async fn idle_since_hint(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        Ok(seat.idle_since_usec)
    }

    #[zbus(property)]
    async fn idle_since_hint_monotonic(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let seat = st.seats.get(&self.seat_id).ok_or_else(|| {
            fdo::Error::Failed("seat not found".to_string())
        })?;
        Ok(seat.idle_since_monotonic)
    }
}
