/// dynamod-hostnamed: hostname1/timedate1/locale1 D-Bus service for dynamoD.
///
/// Provides system identification and configuration interfaces used
/// by GNOME Settings panels.
///
/// Clean-room implementation — no systemd source code was used.
mod hostname;
mod locale;
mod timedate;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-hostnamed starting");

    let connection = zbus::Connection::system().await?;

    // Register hostname1
    connection
        .object_server()
        .at(
            "/org/freedesktop/hostname1",
            hostname::HostnameService,
        )
        .await?;

    connection
        .request_name("org.freedesktop.hostname1")
        .await?;
    tracing::info!("registered org.freedesktop.hostname1");

    // Register timedate1
    connection
        .object_server()
        .at(
            "/org/freedesktop/timedate1",
            timedate::TimedateService,
        )
        .await?;

    connection
        .request_name("org.freedesktop.timedate1")
        .await?;
    tracing::info!("registered org.freedesktop.timedate1");

    // Register locale1
    connection
        .object_server()
        .at(
            "/org/freedesktop/locale1",
            locale::LocaleService,
        )
        .await?;

    connection
        .request_name("org.freedesktop.locale1")
        .await?;
    tracing::info!("registered org.freedesktop.locale1");

    // Send READY=1
    notify_ready();

    tracing::info!("dynamod-hostnamed ready");

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
