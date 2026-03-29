/// Communication channel between svmgr and dynamod-init.
/// Uses a Unix socket pair passed via the DYNAMOD_INIT_FD environment variable.
use std::io::{Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};

use dynamod_common::protocol::{self, Message, MessageBody, MessageKind, ShutdownKind};

static MSG_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    MSG_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A channel to communicate with dynamod-init.
pub struct InitChannel {
    stream: UnixStream,
    read_buf: Vec<u8>,
}

impl InitChannel {
    /// Create from a raw fd (passed via DYNAMOD_INIT_FD).
    pub fn from_fd(fd: RawFd) -> Self {
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        // Set non-blocking for polling reads
        stream.set_nonblocking(true).ok();
        Self {
            stream,
            read_buf: Vec::with_capacity(4096),
        }
    }

    /// Try to create from the environment variable.
    pub fn from_env() -> Option<Self> {
        let fd_str = std::env::var(dynamod_common::paths::INIT_FD_ENV).ok()?;
        let fd: RawFd = fd_str.parse().ok()?;
        Some(Self::from_fd(fd))
    }

    /// Send a heartbeat to init.
    pub fn send_heartbeat(&mut self) -> Result<(), SendError> {
        self.send_message(MessageBody::Heartbeat)
    }

    /// Request a system shutdown.
    pub fn request_shutdown(&mut self, kind: ShutdownKind) -> Result<(), SendError> {
        self.send_message(MessageBody::RequestShutdown { kind })
    }

    /// Log a message via init's kmsg.
    pub fn log_to_kmsg(&mut self, level: u8, message: String) -> Result<(), SendError> {
        self.send_message(MessageBody::LogToKmsg { level, message })
    }

    /// Try to read and decode a message from init (non-blocking).
    pub fn try_recv(&mut self) -> Option<Message> {
        // Read available data
        let mut tmp = [0u8; 4096];
        match self.stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => self.read_buf.extend_from_slice(&tmp[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return None,
        }

        // Try to decode
        match protocol::decode(&self.read_buf) {
            Ok((msg, consumed)) => {
                self.read_buf.drain(..consumed);
                Some(msg)
            }
            Err(protocol::DecodeError::Incomplete) => None,
            Err(_) => {
                // Bad data — clear buffer
                self.read_buf.clear();
                None
            }
        }
    }

    fn send_message(&mut self, body: MessageBody) -> Result<(), SendError> {
        let msg = Message {
            id: next_id(),
            kind: MessageKind::Request,
            body,
        };
        let data = protocol::encode(&msg).map_err(|e| SendError::Encode(e.to_string()))?;
        self.stream
            .write_all(&data)
            .map_err(|e| SendError::Io(e.to_string()))?;
        Ok(())
    }

    /// Get a reference to the underlying stream for polling.
    pub fn as_raw_fd(&self) -> RawFd {
        use std::os::unix::io::AsRawFd;
        self.stream.as_raw_fd()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("encode error: {0}")]
    Encode(String),
    #[error("I/O error: {0}")]
    Io(String),
}
