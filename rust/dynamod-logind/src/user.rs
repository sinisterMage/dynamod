/// org.freedesktop.login1.User D-Bus interface.
///
/// Each logged-in user gets a D-Bus object at
/// /org/freedesktop/login1/user/_<uid>.
///
/// Clean-room implementation based on the freedesktop.org login1 specification.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::{fdo, interface, zvariant};

use crate::manager;
use crate::state::LoginState;

pub struct UserInterface {
    pub uid: u32,
    pub state: Arc<Mutex<LoginState>>,
}

#[interface(name = "org.freedesktop.login1.User")]
impl UserInterface {
    /// Terminate all sessions for this user.
    async fn terminate(&self) -> fdo::Result<()> {
        let mut st = self.state.lock().await;
        let session_ids: Vec<String> = st
            .users
            .get(&self.uid)
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

    /// Kill a specific signal to all processes of this user.
    async fn kill(&self, signal_number: i32) -> fdo::Result<()> {
        let st = self.state.lock().await;
        let session_ids: Vec<String> = st
            .users
            .get(&self.uid)
            .map(|u| u.sessions.clone())
            .unwrap_or_default();

        let sig = nix::sys::signal::Signal::try_from(signal_number)
            .map_err(|_| fdo::Error::InvalidArgs("invalid signal number".to_string()))?;

        for sid in &session_ids {
            if let Some(session) = st.sessions.get(sid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(session.leader_pid as i32),
                    sig,
                );
            }
        }
        Ok(())
    }

    // --- Properties ---

    #[zbus(property, name = "UID")]
    async fn uid_prop(&self) -> u32 {
        self.uid
    }

    #[zbus(property, name = "GID")]
    async fn gid(&self) -> u32 {
        // Look up primary GID from passwd
        nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(self.uid))
            .ok()
            .flatten()
            .map(|u| u.gid.as_raw())
            .unwrap_or(self.uid)
    }

    #[zbus(property)]
    async fn name(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        Ok(user.name.clone())
    }

    #[zbus(property)]
    async fn runtime_path(&self) -> String {
        format!("/run/user/{}", self.uid)
    }

    #[zbus(property)]
    async fn display(
        &self,
    ) -> fdo::Result<(String, zvariant::OwnedObjectPath)> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        match &user.display_session {
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
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        let mut result = Vec::new();
        for sid in &user.sessions {
            let path =
                zvariant::OwnedObjectPath::try_from(manager::session_path(sid))
                    .map_err(|e| fdo::Error::Failed(e.to_string()))?;
            result.push((sid.clone(), path));
        }
        Ok(result)
    }

    #[zbus(property)]
    async fn state(&self) -> fdo::Result<String> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        Ok(user.state.as_str().to_string())
    }

    #[zbus(property)]
    async fn idle_hint(&self) -> fdo::Result<bool> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        Ok(user.idle_hint)
    }

    #[zbus(property)]
    async fn idle_since_hint(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        Ok(user.idle_since_usec)
    }

    #[zbus(property)]
    async fn idle_since_hint_monotonic(&self) -> fdo::Result<u64> {
        let st = self.state.lock().await;
        let user = st.users.get(&self.uid).ok_or_else(|| {
            fdo::Error::Failed("user not found".to_string())
        })?;
        Ok(user.idle_since_monotonic)
    }

    #[zbus(property)]
    async fn linger(&self) -> bool {
        false
    }
}
