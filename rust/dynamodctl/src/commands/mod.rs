/// dynamodctl command implementations.
///
/// Each command connects to the control socket, sends a request,
/// and prints the response.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use dynamod_common::protocol::{
    self, Message, MessageBody, MessageKind, ShutdownKind,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

/// Send a request to the control socket and return the response body.
fn send_request(socket_path: &str, body: MessageBody) -> std::result::Result<MessageBody, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("cannot connect to {socket_path}: {e}"))?;

    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;

    let msg = Message {
        id: 1,
        kind: MessageKind::Request,
        body,
    };

    let data = protocol::encode(&msg)?;
    stream.write_all(&data)?;

    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Err("empty response from server".into());
    }

    let (resp, _) = protocol::decode(&buf[..n])?;
    Ok(resp.body)
}

pub fn start(socket_path: &str, name: &str) -> Result {
    match send_request(socket_path, MessageBody::StartService { name: name.to_string() })? {
        MessageBody::Ack => {
            println!("Starting service '{name}'...");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn stop(socket_path: &str, name: &str) -> Result {
    match send_request(socket_path, MessageBody::StopService { name: name.to_string() })? {
        MessageBody::Ack => {
            println!("Stopping service '{name}'...");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn restart(socket_path: &str, name: &str) -> Result {
    match send_request(socket_path, MessageBody::RestartService { name: name.to_string() })? {
        MessageBody::Ack => {
            println!("Restarting service '{name}'...");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn status(socket_path: &str, name: &str) -> Result {
    match send_request(socket_path, MessageBody::ServiceStatus { name: name.to_string() })? {
        MessageBody::ServiceInfo { name, status, pid, supervisor } => {
            println!("Service: {name}");
            println!("  Status:     {status}");
            if let Some(p) = pid {
                println!("  PID:        {p}");
            }
            println!("  Supervisor: {supervisor}");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn list(socket_path: &str) -> Result {
    match send_request(socket_path, MessageBody::ListServices)? {
        MessageBody::ServiceList { services } => {
            if services.is_empty() {
                println!("No services configured.");
                return Ok(());
            }

            // Print table header
            println!("{:<30} {:<15} {:<10}", "SERVICE", "STATUS", "PID");
            println!("{}", "-".repeat(55));

            let mut services = services;
            services.sort_by(|a, b| a.name.cmp(&b.name));

            for svc in &services {
                let pid_str = svc.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
                println!("{:<30} {:<15} {:<10}", svc.name, svc.status, pid_str);
            }

            println!("\n{} service(s) total", services.len());
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn tree(socket_path: &str) -> Result {
    match send_request(socket_path, MessageBody::TreeStatus)? {
        MessageBody::Tree { text } => {
            print!("{text}");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn reload(socket_path: &str) -> Result {
    match send_request(socket_path, MessageBody::Reload)? {
        MessageBody::Ack => {
            println!("Reloading service definitions...");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

pub fn shutdown(socket_path: &str, kind: &str) -> Result {
    let shutdown_kind = match kind {
        "poweroff" | "off" => ShutdownKind::Poweroff,
        "reboot" => ShutdownKind::Reboot,
        "halt" => ShutdownKind::Halt,
        _ => return Err(format!("unknown shutdown kind: {kind} (use poweroff, reboot, or halt)").into()),
    };

    match send_request(socket_path, MessageBody::Shutdown { kind: shutdown_kind })? {
        MessageBody::Ack => {
            println!("System {kind} initiated.");
            Ok(())
        }
        MessageBody::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}
