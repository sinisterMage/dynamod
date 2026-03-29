/// Client for communicating with dynamod-svmgr via the control socket.
///
/// Used to forward shutdown/reboot requests from logind to the service manager.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use dynamod_common::protocol::{self, Message, MessageBody, MessageKind, ShutdownKind};

const CONTROL_SOCK: &str = dynamod_common::paths::CONTROL_SOCK;

#[derive(Debug, thiserror::Error)]
pub enum SvmgrError {
    #[error("failed to connect to control socket: {0}")]
    Connect(std::io::Error),
    #[error("failed to send message: {0}")]
    Send(std::io::Error),
    #[error("failed to read response: {0}")]
    Recv(std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Send a shutdown request to dynamod-svmgr.
pub fn request_shutdown(kind: ShutdownKind) -> Result<(), SvmgrError> {
    send_command(MessageBody::Shutdown { kind })
}

/// Send a command to svmgr and wait for the response.
fn send_command(body: MessageBody) -> Result<(), SvmgrError> {
    let mut stream =
        UnixStream::connect(CONTROL_SOCK).map_err(SvmgrError::Connect)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(SvmgrError::Send)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(SvmgrError::Send)?;

    let msg = Message {
        id: 1,
        kind: MessageKind::Request,
        body,
    };

    let data = protocol::encode(&msg).map_err(|e| SvmgrError::Protocol(e.to_string()))?;
    stream.write_all(&data).map_err(SvmgrError::Send)?;

    // Read response
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).map_err(SvmgrError::Recv)?;
    if n == 0 {
        return Err(SvmgrError::Protocol("empty response".to_string()));
    }

    let (resp, _) = protocol::decode(&buf[..n])
        .map_err(|e| SvmgrError::Protocol(e.to_string()))?;

    match resp.body {
        MessageBody::Ack => Ok(()),
        MessageBody::Error { message } => {
            Err(SvmgrError::Protocol(message))
        }
        _ => Err(SvmgrError::Protocol("unexpected response".to_string())),
    }
}
