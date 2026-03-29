/// dynamod-logind: login1 D-Bus service for dynamoD.
///
/// Provides the org.freedesktop.login1 D-Bus interface for session, seat,
/// and user management. This enables Wayland compositors and desktop
/// environments to manage device access and sessions without systemd.
///
/// Clean-room implementation — no systemd source code was used.
mod auth;
mod device;
mod inhibitor;
mod manager;
mod seat;
mod session;
mod state;
mod svmgr_client;
mod user;
mod vtswitch;

use std::sync::Arc;
use tokio::sync::Mutex;

use state::LoginState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-logind starting");

    // Initialize login state with seat0
    let login_state = Arc::new(Mutex::new(LoginState::new()));
    {
        let mut st = login_state.lock().await;
        st.create_seat0();
    }

    // Detect graphical capability
    detect_graphics().await;

    // Connect to the system D-Bus
    let connection = zbus::Connection::system().await?;

    // We need the object server reference for dynamic object registration.
    // Store it so the Manager can register Session/User objects at runtime.
    let object_server = Arc::new(Mutex::new(None::<zbus::ObjectServer>));

    // Create the Manager interface
    let mgr = manager::Manager {
        state: Arc::clone(&login_state),
        object_server: Arc::clone(&object_server),
    };

    // Register the seat0 D-Bus object
    let seat0_iface = seat::SeatInterface {
        seat_id: "seat0".to_string(),
        state: Arc::clone(&login_state),
    };

    // Serve interfaces on the connection
    connection
        .object_server()
        .at("/org/freedesktop/login1", mgr)
        .await?;

    connection
        .object_server()
        .at("/org/freedesktop/login1/seat/seat0", seat0_iface)
        .await?;

    // Store the object server reference for dynamic registration
    {
        let mut os = object_server.lock().await;
        *os = Some(connection.object_server().clone());
    }

    // Request the well-known bus name
    connection
        .request_name("org.freedesktop.login1")
        .await?;

    tracing::info!("registered on D-Bus as org.freedesktop.login1");

    // Signal readiness to dynamod-svmgr via sd_notify protocol
    notify_ready();

    // Spawn inhibitor garbage collection task
    let gc_state = Arc::clone(&login_state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let mut st = gc_state.lock().await;
            st.inhibitors.retain(|i| !i.is_released());
        }
    });

    tracing::info!("dynamod-logind ready, waiting for D-Bus requests");

    // Run forever, processing D-Bus messages
    std::future::pending::<()>().await;

    Ok(())
}

/// Detect if the system has graphical capability by checking for DRM devices.
async fn detect_graphics() {
    match std::fs::read_dir("/sys/class/drm") {
        Ok(entries) => {
            let has_card = entries
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.file_name()
                        .to_str()
                        .map_or(false, |n| n.starts_with("card"))
                });
            if has_card {
                tracing::info!("graphics hardware detected");
            } else {
                tracing::warn!("no DRM card devices found");
            }
        }
        Err(e) => {
            tracing::warn!("cannot read /sys/class/drm: {e}");
        }
    }
}

/// Send READY=1 to the NOTIFY_SOCKET if set (sd_notify compatible).
fn notify_ready() {
    if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
        let path = if let Some(stripped) = socket_path.strip_prefix('@') {
            // Abstract socket
            format!("\0{stripped}")
        } else {
            socket_path.clone()
        };
        match std::os::unix::net::UnixDatagram::unbound() {
            Ok(sock) => {
                if sock.send_to(b"READY=1", &path).is_ok() {
                    tracing::info!("sent READY=1 to notify socket");
                }
            }
            Err(e) => {
                tracing::warn!("failed to create notify socket: {e}");
            }
        }
    }
}
