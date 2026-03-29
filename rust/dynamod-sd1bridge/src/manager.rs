/// org.freedesktop.systemd1.Manager D-Bus interface.
///
/// Translates systemd1 D-Bus method calls to dynamod's native IPC protocol.
/// Clean-room implementation from freedesktop.org specs.
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, zvariant};

use crate::mapping;
use crate::SvmgrClient;

pub struct SystemdManager {
    pub client: Arc<Mutex<SvmgrClient>>,
    pub object_server: Arc<Mutex<Option<zbus::ObjectServer>>>,
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl SystemdManager {
    /// Start a unit.
    async fn start_unit(
        &self,
        name: &str,
        _mode: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(name);
        let mut client = self.client.lock().await;
        client
            .start_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(name);
        zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Stop a unit.
    async fn stop_unit(
        &self,
        name: &str,
        _mode: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(name);
        let mut client = self.client.lock().await;
        client
            .stop_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(name);
        zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Restart a unit.
    async fn restart_unit(
        &self,
        name: &str,
        _mode: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(name);
        let mut client = self.client.lock().await;
        client
            .restart_service(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let path = mapping::unit_object_path(name);
        zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Get the D-Bus object path for a unit.
    async fn get_unit(
        &self,
        name: &str,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let service = mapping::unit_to_service(name);
        let mut client_guard = self.client.lock().await;

        // Verify the service exists
        client_guard
            .service_status(service)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        // Register a Unit D-Bus object dynamically
        let unit_name = mapping::service_to_unit(service);
        let path = mapping::unit_object_path(&unit_name);

        if let Some(ref server) = *self.object_server.lock().await {
            let unit_iface = crate::unit::UnitInterface {
                unit_name: unit_name.clone(),
                client: Arc::clone(&self.client),
            };
            let _ = server.at(path.as_str(), unit_iface).await;
        }

        zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Get a unit by the PID of one of its processes.
    async fn get_unit_by_pid(
        &self,
        pid: u32,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let mut client = self.client.lock().await;
        let name = client
            .service_by_pid(pid as i32)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        match name {
            Some(n) => {
                let unit_name = mapping::service_to_unit(&n);
                let path = mapping::unit_object_path(&unit_name);
                zvariant::OwnedObjectPath::try_from(path)
                    .map_err(|e| fdo::Error::Failed(e.to_string()))
            }
            None => Err(fdo::Error::Failed(format!(
                "no unit for PID {pid}"
            ))),
        }
    }

    /// List all loaded units.
    /// Returns: array of (name, description, load_state, active_state,
    ///          sub_state, following, unit_path, job_id, job_type, job_path)
    #[allow(clippy::type_complexity)]
    async fn list_units(
        &self,
    ) -> fdo::Result<
        Vec<(
            String,                     // name
            String,                     // description
            String,                     // load_state
            String,                     // active_state
            String,                     // sub_state
            String,                     // following
            zvariant::OwnedObjectPath,  // unit_path
            u32,                        // job_id
            String,                     // job_type
            zvariant::OwnedObjectPath,  // job_path
        )>,
    > {
        let mut client = self.client.lock().await;
        let services = client
            .list_services()
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        let mut result = Vec::new();
        let empty_job_path = zvariant::OwnedObjectPath::try_from("/")
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        for svc in services {
            let unit_name = mapping::service_to_unit(&svc.name);
            let (active, sub) = mapping::map_status(&svc.status);
            let path = zvariant::OwnedObjectPath::try_from(
                mapping::unit_object_path(&unit_name),
            )
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;

            result.push((
                unit_name,
                format!("dynamod service: {}", svc.name),
                "loaded".to_string(),
                active.to_string(),
                sub.to_string(),
                String::new(),
                path,
                0u32,
                String::new(),
                empty_job_path.clone(),
            ));
        }

        Ok(result)
    }

    /// Reload the daemon configuration.
    async fn reload(&self) -> fdo::Result<()> {
        let mut client = self.client.lock().await;
        client
            .reload()
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Initiate system power-off.
    async fn power_off(&self) -> fdo::Result<()> {
        let mut client = self.client.lock().await;
        client
            .shutdown(dynamod_common::protocol::ShutdownKind::Poweroff)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Initiate system reboot.
    async fn reboot(&self) -> fdo::Result<()> {
        let mut client = self.client.lock().await;
        client
            .shutdown(dynamod_common::protocol::ShutdownKind::Reboot)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    /// Initiate system halt.
    async fn halt(&self) -> fdo::Result<()> {
        let mut client = self.client.lock().await;
        client
            .shutdown(dynamod_common::protocol::ShutdownKind::Halt)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    // --- Signals ---

    #[zbus(signal)]
    async fn unit_new(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unit_removed(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit_path: zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn job_new(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job_path: zvariant::ObjectPath<'_>,
        unit: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job_path: zvariant::ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    // --- Properties ---

    #[zbus(property)]
    async fn version(&self) -> String {
        "dynamod 0.1.0".to_string()
    }

    #[zbus(property)]
    async fn features(&self) -> String {
        "+systemd-mimic".to_string()
    }

    #[zbus(property)]
    async fn architecture(&self) -> String {
        std::env::consts::ARCH.to_string()
    }

    #[zbus(property)]
    async fn virtualization(&self) -> String {
        // Simple virtualization detection
        std::fs::read_to_string("/sys/class/dmi/id/product_name")
            .map(|s| {
                let s = s.trim().to_lowercase();
                if s.contains("virtualbox") {
                    "oracle"
                } else if s.contains("vmware") {
                    "vmware"
                } else if s.contains("qemu") || s.contains("kvm") {
                    "kvm"
                } else if s.contains("hyper-v") {
                    "microsoft"
                } else {
                    ""
                }
            })
            .unwrap_or_default()
            .to_string()
    }

    #[zbus(property)]
    async fn control_group(&self) -> String {
        "/sys/fs/cgroup/dynamod".to_string()
    }

    #[zbus(property)]
    async fn system_state(&self) -> String {
        "running".to_string()
    }

    #[zbus(property)]
    async fn default_target(&self) -> String {
        "multi-user.target".to_string()
    }

    #[zbus(property, name = "NNames")]
    async fn n_names(&self) -> fdo::Result<u32> {
        let mut client = self.client.lock().await;
        let services = client
            .list_services()
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok(services.len() as u32)
    }

    #[zbus(property, name = "NInstalledJobs")]
    async fn n_installed_jobs(&self) -> u32 {
        0
    }

    #[zbus(property, name = "NFailedJobs")]
    async fn n_failed_jobs(&self) -> u32 {
        0
    }

    #[zbus(property, name = "NFailedUnits")]
    async fn n_failed_units(&self) -> fdo::Result<u32> {
        let mut client = self.client.lock().await;
        let services = client
            .list_services()
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok(services.iter().filter(|s| s.status == "failed").count() as u32)
    }
}
