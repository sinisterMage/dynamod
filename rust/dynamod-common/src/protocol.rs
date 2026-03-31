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

    // === dynamodctl / bridge <-> svmgr (control socket) ===
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
    /// Look up the service that owns a given PID.
    GetServiceByPid { pid: i32 },

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
    /// Response to GetServiceByPid.
    ServiceByPid {
        name: Option<String>,
        pid: i32,
    },
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

/// Protocol version. Currently unused in the wire format but reserved
/// for future backward-compatible negotiation.
pub const PROTOCOL_VERSION: u8 = 1;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(body: MessageBody) -> Message {
        Message {
            id: 42,
            kind: MessageKind::Request,
            body,
        }
    }

    #[test]
    fn encode_decode_roundtrip_heartbeat() {
        let msg = make_msg(MessageBody::Heartbeat);
        let encoded = encode(&msg).unwrap();

        assert_eq!(&encoded[..2], &MAGIC);

        let (decoded, consumed) = decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.id, 42);
        assert!(matches!(decoded.body, MessageBody::Heartbeat));
    }

    #[test]
    fn encode_decode_roundtrip_heartbeat_ack() {
        let msg = Message {
            id: 1,
            kind: MessageKind::Response { in_reply_to: 1 },
            body: MessageBody::HeartbeatAck,
        };
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        assert!(matches!(decoded.kind, MessageKind::Response { in_reply_to: 1 }));
        assert!(matches!(decoded.body, MessageBody::HeartbeatAck));
    }

    #[test]
    fn encode_decode_roundtrip_shutdown() {
        for kind in [ShutdownKind::Poweroff, ShutdownKind::Reboot, ShutdownKind::Halt] {
            let msg = make_msg(MessageBody::RequestShutdown { kind: kind.clone() });
            let encoded = encode(&msg).unwrap();
            let (decoded, _) = decode(&encoded).unwrap();
            match decoded.body {
                MessageBody::RequestShutdown { kind: dk } => {
                    assert_eq!(format!("{dk:?}"), format!("{kind:?}"));
                }
                other => panic!("expected RequestShutdown, got {other:?}"),
            }
        }
    }

    #[test]
    fn encode_decode_roundtrip_service_ops() {
        let msg = make_msg(MessageBody::StartService {
            name: "postgresql".to_string(),
        });
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::StartService { name } => assert_eq!(name, "postgresql"),
            other => panic!("expected StartService, got {other:?}"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_service_list() {
        let entries = vec![
            ServiceEntry {
                name: "sshd".to_string(),
                status: "running".to_string(),
                pid: Some(1234),
            },
            ServiceEntry {
                name: "network".to_string(),
                status: "stopped".to_string(),
                pid: None,
            },
        ];
        let msg = make_msg(MessageBody::ServiceList {
            services: entries.clone(),
        });
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::ServiceList { services } => {
                assert_eq!(services.len(), 2);
                assert_eq!(services[0].name, "sshd");
                assert_eq!(services[0].pid, Some(1234));
                assert_eq!(services[1].name, "network");
                assert_eq!(services[1].pid, None);
            }
            other => panic!("expected ServiceList, got {other:?}"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_log_to_kmsg() {
        let msg = make_msg(MessageBody::LogToKmsg {
            level: 6,
            message: "hello from svmgr".to_string(),
        });
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::LogToKmsg { level, message } => {
                assert_eq!(level, 6);
                assert_eq!(message, "hello from svmgr");
            }
            other => panic!("expected LogToKmsg, got {other:?}"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_error() {
        let msg = make_msg(MessageBody::Error {
            message: "unknown service: foo".to_string(),
        });
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::Error { message } => {
                assert_eq!(message, "unknown service: foo");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn decode_incomplete_header() {
        let buf = [0x44, 0x4D]; // only magic, no length
        assert!(matches!(decode(&buf), Err(DecodeError::Incomplete)));
    }

    #[test]
    fn decode_empty() {
        assert!(matches!(decode(&[]), Err(DecodeError::Incomplete)));
    }

    #[test]
    fn decode_bad_magic() {
        let buf = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
        assert!(matches!(decode(&buf), Err(DecodeError::BadMagic)));
    }

    #[test]
    fn decode_too_large() {
        let mut buf = vec![0x44, 0x4D];
        let huge_len = (MAX_MESSAGE_SIZE + 1) as u32;
        buf.extend_from_slice(&huge_len.to_le_bytes());
        assert!(matches!(decode(&buf), Err(DecodeError::TooLarge(_))));
    }

    #[test]
    fn decode_incomplete_payload() {
        let msg = make_msg(MessageBody::Heartbeat);
        let encoded = encode(&msg).unwrap();
        // Give it only part of the payload
        let partial = &encoded[..encoded.len() - 1];
        assert!(matches!(decode(partial), Err(DecodeError::Incomplete)));
    }

    #[test]
    fn wire_format_structure() {
        let msg = make_msg(MessageBody::Heartbeat);
        let encoded = encode(&msg).unwrap();

        // Verify header structure
        assert_eq!(encoded[0], 0x44); // 'D'
        assert_eq!(encoded[1], 0x4D); // 'M'

        let payload_len =
            u32::from_le_bytes([encoded[2], encoded[3], encoded[4], encoded[5]]) as usize;
        assert_eq!(encoded.len(), HEADER_SIZE + payload_len);
    }

    #[test]
    fn decode_consumes_correct_bytes() {
        let msg1 = make_msg(MessageBody::Heartbeat);
        let msg2 = make_msg(MessageBody::HeartbeatAck);
        let enc1 = encode(&msg1).unwrap();
        let enc2 = encode(&msg2).unwrap();

        let mut combined = enc1.clone();
        combined.extend_from_slice(&enc2);

        let (decoded1, consumed1) = decode(&combined).unwrap();
        assert_eq!(consumed1, enc1.len());
        assert!(matches!(decoded1.body, MessageBody::Heartbeat));

        let (decoded2, consumed2) = decode(&combined[consumed1..]).unwrap();
        assert_eq!(consumed2, enc2.len());
        assert!(matches!(decoded2.body, MessageBody::HeartbeatAck));
    }

    #[test]
    fn encode_decode_all_message_kinds() {
        let kinds = vec![
            MessageKind::Request,
            MessageKind::Response { in_reply_to: 99 },
            MessageKind::Event,
        ];
        for kind in kinds {
            let msg = Message {
                id: 7,
                kind: kind.clone(),
                body: MessageBody::Ack,
            };
            let encoded = encode(&msg).unwrap();
            let (decoded, _) = decode(&encoded).unwrap();
            assert_eq!(format!("{:?}", decoded.kind), format!("{kind:?}"));
        }
    }

    #[test]
    fn encode_decode_get_service_by_pid() {
        let msg = make_msg(MessageBody::GetServiceByPid { pid: 1234 });
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::GetServiceByPid { pid } => assert_eq!(pid, 1234),
            other => panic!("expected GetServiceByPid, got {other:?}"),
        }
    }

    #[test]
    fn encode_decode_service_by_pid_response() {
        let msg = Message {
            id: 10,
            kind: MessageKind::Response { in_reply_to: 9 },
            body: MessageBody::ServiceByPid {
                name: Some("sshd".to_string()),
                pid: 1234,
            },
        };
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode(&encoded).unwrap();
        match decoded.body {
            MessageBody::ServiceByPid { name, pid } => {
                assert_eq!(name.as_deref(), Some("sshd"));
                assert_eq!(pid, 1234);
            }
            other => panic!("expected ServiceByPid, got {other:?}"),
        }
    }
}
