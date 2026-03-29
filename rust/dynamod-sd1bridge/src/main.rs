/// dynamod-sd1bridge: systemd1 D-Bus bridge for dynamoD.
///
/// Translates org.freedesktop.systemd1 D-Bus calls to dynamod's native
/// IPC protocol over the control socket. This makes systemctl and GNOME
/// service management tools work with dynamoD.
///
/// Clean-room implementation — no systemd source code was used.
mod manager;
mod mapping;
mod unit;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use dynamod_common::protocol::{
    self, Message, MessageBody, MessageKind, ServiceEntry, ShutdownKind,
};

/// Client for communicating with dynamod-svmgr via the control socket.
pub struct SvmgrClient {
    next_id: u64,
}

/// Response from a service status query.
pub struct ServiceInfo {
    pub name: String,
    pub status: String,
    pub pid: Option<i32>,
    pub supervisor: String,
}

impl SvmgrClient {
    fn new() -> Self {
        Self { next_id: 1 }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send_command(&mut self, body: MessageBody) -> Result<MessageBody, String> {
        let id = self.next_id();
        let msg = Message {
            id,
            kind: MessageKind::Request,
            body,
        };

        // Use blocking I/O in a spawn_blocking context
        let result = tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(dynamod_common::paths::CONTROL_SOCK)
                .map_err(|e| format!("connect: {e}"))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| format!("timeout: {e}"))?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| format!("timeout: {e}"))?;

            let data = protocol::encode(&msg).map_err(|e| format!("encode: {e}"))?;
            stream.write_all(&data).map_err(|e| format!("write: {e}"))?;

            let mut buf = vec![0u8; 65536];
            let n = stream.read(&mut buf).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("empty response".to_string());
            }
            let (resp, _) =
                protocol::decode(&buf[..n]).map_err(|e| format!("decode: {e}"))?;
            Ok(resp.body)
        })
        .await
        .map_err(|e| format!("spawn: {e}"))?;

        result
    }

    pub async fn start_service(&mut self, name: &str) -> Result<(), String> {
        match self.send_command(MessageBody::StartService { name: name.to_string() }).await? {
            MessageBody::Ack => Ok(()),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn stop_service(&mut self, name: &str) -> Result<(), String> {
        match self.send_command(MessageBody::StopService { name: name.to_string() }).await? {
            MessageBody::Ack => Ok(()),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn restart_service(&mut self, name: &str) -> Result<(), String> {
        match self.send_command(MessageBody::RestartService { name: name.to_string() }).await? {
            MessageBody::Ack => Ok(()),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn service_status(&mut self, name: &str) -> Result<ServiceInfo, String> {
        match self.send_command(MessageBody::ServiceStatus { name: name.to_string() }).await? {
            MessageBody::ServiceInfo { name, status, pid, supervisor } => {
                Ok(ServiceInfo { name, status, pid, supervisor })
            }
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn list_services(&mut self) -> Result<Vec<ServiceEntry>, String> {
        match self.send_command(MessageBody::ListServices).await? {
            MessageBody::ServiceList { services } => Ok(services),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn service_by_pid(&mut self, pid: i32) -> Result<Option<String>, String> {
        match self.send_command(MessageBody::GetServiceByPid { pid }).await? {
            MessageBody::ServiceByPid { name, .. } => Ok(name),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn reload(&mut self) -> Result<(), String> {
        match self.send_command(MessageBody::Reload).await? {
            MessageBody::Ack => Ok(()),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }

    pub async fn shutdown(&mut self, kind: ShutdownKind) -> Result<(), String> {
        match self.send_command(MessageBody::Shutdown { kind }).await? {
            MessageBody::Ack => Ok(()),
            MessageBody::Error { message } => Err(message),
            _ => Err("unexpected response".to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-sd1bridge starting");

    let client = Arc::new(Mutex::new(SvmgrClient::new()));
    let object_server = Arc::new(Mutex::new(None::<zbus::ObjectServer>));

    let connection = zbus::Connection::system().await?;

    let mgr = manager::SystemdManager {
        client: Arc::clone(&client),
        object_server: Arc::clone(&object_server),
    };

    connection
        .object_server()
        .at("/org/freedesktop/systemd1", mgr)
        .await?;

    {
        let mut os = object_server.lock().await;
        *os = Some(connection.object_server().clone());
    }

    connection
        .request_name("org.freedesktop.systemd1")
        .await?;

    tracing::info!("registered on D-Bus as org.freedesktop.systemd1");

    // Send READY=1
    notify_ready();

    tracing::info!("dynamod-sd1bridge ready");

    std::future::pending::<()>().await;

    Ok(())
}

fn notify_ready() {
    if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
        let path = if let Some(stripped) = socket_path.strip_prefix('@') {
            format!("\0{stripped}")
        } else {
            socket_path
        };
        if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
            let _ = sock.send_to(b"READY=1", &path);
        }
    }
}
