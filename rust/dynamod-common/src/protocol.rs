/// IPC protocol definitions shared between all dynamod components.
///
/// Wire format: [magic: 0x444D (2B)] [length: u32 LE (4B)] [payload: MessagePack]
use serde::{Deserialize, Serialize};

/// Magic bytes identifying a dynamod IPC message.
pub const MAGIC: [u8; 2] = [0x44, 0x4D];

/// Maximum message payload size (64 KiB).
pub const MAX_MESSAGE_SIZE: u32 = 64 * 1024;

/// Header size: 2 bytes magic + 4 bytes length.
pub const HEADER_SIZE: usize = 6;

/// A message envelope with an ID and kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: u64,
    pub kind: MessageKind,
    pub body: MessageBody,
}

/// Whether this is a request, response, or unsolicited event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageKind {
    Request,
    Response { in_reply_to: u64 },
    Event,
}

/// Message body variants for all IPC channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageBody {
    // === init <-> svmgr ===
    /// svmgr -> init: Request system shutdown.
    RequestShutdown { kind: ShutdownKind },
    /// svmgr -> init: Heartbeat (svmgr is alive).
    Heartbeat,
    /// init -> svmgr: Acknowledge heartbeat.
    HeartbeatAck,
    /// init -> svmgr: Shutdown signal received from kernel.
    ShutdownSignal { signal: String },
    /// svmgr -> init: Write to kernel log.
    LogToKmsg { level: u8, message: String },

    // === dynamodctl <-> svmgr (control socket) ===
    /// Start a service by name.
    StartService { name: String },
    /// Stop a service by name.
    StopService { name: String },
    /// Restart a service by name.
    RestartService { name: String },
    /// Query status of a single service.
    ServiceStatus { name: String },
    /// List all services and their statuses.
    ListServices,
    /// Get the full supervisor tree.
    TreeStatus,
    /// Reload configuration files.
    Reload,
    /// Initiate system shutdown.
    Shutdown { kind: ShutdownKind },

    // === Responses ===
    /// Generic acknowledgment.
    Ack,
    /// Generic error.
    Error { message: String },
    /// Single service status response.
    ServiceInfo {
        name: String,
        status: String,
        pid: Option<i32>,
        supervisor: String,
    },
    /// List of services response.
    ServiceList { services: Vec<ServiceEntry> },
    /// Supervisor tree response.
    Tree { text: String },
}

/// A service entry in a list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEntry {
    pub name: String,
    pub status: String,
    pub pid: Option<i32>,
}

/// Shutdown kind for the RequestShutdown message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShutdownKind {
    Poweroff,
    Reboot,
    Halt,
}

/// Encode a message into the wire format (magic + length + msgpack).
/// Uses struct-as-map serialization so fields have string keys,
/// which the Zig msgpack decoder can look up by name.
pub fn encode(msg: &Message) -> Result<Vec<u8>, EncodeError> {
    let mut buf = Vec::new();
    let mut serializer = rmp_serde::Serializer::new(&mut buf).with_struct_map();
    serde::Serialize::serialize(msg, &mut serializer)
        .map_err(EncodeError::Serialize)?;
    let payload = buf;

    if payload.len() > MAX_MESSAGE_SIZE as usize {
        return Err(EncodeError::TooLarge(payload.len()));
    }

    let mut buf = Vec::with_capacity(HEADER_SIZE + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// Decode a message from a buffer that starts with the magic bytes.
/// Returns the message and number of bytes consumed.
pub fn decode(buf: &[u8]) -> Result<(Message, usize), DecodeError> {
    if buf.len() < HEADER_SIZE {
        return Err(DecodeError::Incomplete);
    }

    if buf[0..2] != MAGIC {
        return Err(DecodeError::BadMagic);
    }

    let len = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) as usize;
    if len > MAX_MESSAGE_SIZE as usize {
        return Err(DecodeError::TooLarge(len));
    }

    let total = HEADER_SIZE + len;
    if buf.len() < total {
        return Err(DecodeError::Incomplete);
    }

    let msg =
        rmp_serde::from_slice(&buf[HEADER_SIZE..total]).map_err(DecodeError::Deserialize)?;
    Ok((msg, total))
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("serialization failed: {0}")]
    Serialize(rmp_serde::encode::Error),
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("incomplete message")]
    Incomplete,
    #[error("bad magic bytes")]
    BadMagic,
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
    #[error("deserialization failed: {0}")]
    Deserialize(rmp_serde::decode::Error),
}
