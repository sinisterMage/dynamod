/// org.freedesktop.systemd1.Unit D-Bus interface.
///
/// Each dynamod service is exposed as a systemd unit object.
/// Clean-room implementation from freedesktop.org specs.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::{fdo, interface};

use crate::mapping;
use crate::SvmgrClient;

pub struct UnitInterface {
    /// The systemd-style unit name (e.g., "sshd.service").
    pub unit_name: String,
    pub client: Arc<Mutex<SvmgrClient>>,
}

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl UnitInterface {
    async fn start(&self, _mode: &str) -> fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        client
            .start_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(&self.unit_name);
        zbus::zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn stop(&self, _mode: &str) -> fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        client
            .stop_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(&self.unit_name);
        zbus::zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn restart(&self, _mode: &str) -> fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        client
            .restart_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(&self.unit_name);
        zbus::zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    // --- Properties ---

    #[zbus(property)]
    async fn id(&self) -> String {
        self.unit_name.clone()
    }

    #[zbus(property)]
    async fn names(&self) -> Vec<String> {
        vec![self.unit_name.clone()]
    }

    #[zbus(property)]
    async fn description(&self) -> String {
        format!(
            "dynamod service: {}",
            mapping::unit_to_service(&self.unit_name)
        )
    }

    #[zbus(property)]
    async fn load_state(&self) -> String {
        "loaded".to_string()
    }

    #[zbus(property)]
    async fn active_state(&self) -> fdo::Result<String> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        let info = client
            .service_status(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let (active, _) = mapping::map_status(&info.status);
        Ok(active.to_string())
    }

    #[zbus(property)]
    async fn sub_state(&self) -> fdo::Result<String> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        let info = client
            .service_status(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let (_, sub) = mapping::map_status(&info.status);
        Ok(sub.to_string())
    }

    #[zbus(property, name = "MainPID")]
    async fn main_pid(&self) -> fdo::Result<u32> {
        let service = mapping::unit_to_service(&self.unit_name);
        let mut client = self.client.lock().await;
        let info = client
            .service_status(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok(info.pid.unwrap_or(0) as u32)
    }

    #[zbus(property)]
    async fn fragment_path(&self) -> String {
        let service = mapping::unit_to_service(&self.unit_name);
        format!("/etc/dynamod/services/{service}.toml")
    }

    #[zbus(property)]
    async fn can_start(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn can_stop(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn can_restart(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn can_reload(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn can_isolate(&self) -> bool {
        false
    }
}
