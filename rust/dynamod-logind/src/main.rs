/// dynamod-logind: login1 D-Bus service for dynamoD.
///
/// Provides the org.freedesktop.login1 D-Bus interface for session, seat,
/// and user management. This enables Wayland compositors and desktop
/// environments to manage device access and sessions without systemd.
///
/// Clean-room implementation — no systemd source code was used.
mod acpi;
mod auth;
mod cgroup;
mod config;
mod device;
mod inhibitor;
mod manager;
mod power;
mod runtime_dir;
mod seat;
mod session;
mod state;
mod svmgr_client;
mod user;
mod vtswitch;

use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use state::LoginState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-logind starting");

    // Load /etc/dynamod/logind.conf (uses defaults on miss).
    let config = Arc::new(RwLock::new(config::Config::load()));

    // Initialize login state with seat0
    let login_state = Arc::new(Mutex::new(LoginState::with_config(Arc::clone(&config))));
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
        connection: connection.clone(),
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

    // Reload config on SIGHUP.
    let cfg_for_reload = Arc::clone(&config);
    tokio::spawn(async move {
        let mut hup = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::hangup(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot install SIGHUP handler: {e}");
                return;
            }
        };
        while hup.recv().await.is_some() {
            let new_cfg = config::Config::load();
            *cfg_for_reload.write().await = new_cfg;
            tracing::info!("reloaded logind.conf");
        }
    });

    // Start the ACPI event source (lid / power-button / sleep-button).
    let emit_sleep = prepare_for_sleep_emitter(connection.clone());
    let emit_shutdown = prepare_for_shutdown_emitter(connection.clone());
    acpi::spawn(Arc::clone(&login_state), emit_sleep, emit_shutdown);

    tracing::info!("dynamod-logind ready, waiting for D-Bus requests");

    // Run forever, processing D-Bus messages
    std::future::pending::<()>().await;

    Ok(())
}

fn prepare_for_sleep_emitter(conn: zbus::Connection) -> acpi::PrepareEmitter {
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

fn prepare_for_shutdown_emitter(conn: zbus::Connection) -> acpi::PrepareEmitter {
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
