/// Control socket server for dynamodctl <-> svmgr communication.
///
/// Listens on /run/dynamod/control.sock for commands from dynamodctl.
/// Each connection handles one request-response exchange.
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use dynamod_common::protocol::{
    self, Message, MessageBody, MessageKind, ServiceEntry,
};

use crate::supervisor::lifecycle::ServiceState;
use crate::supervisor::tree::{SupervisorTree, TreeNode};

/// The control socket server.
pub struct ControlServer {
    listener: UnixListener,
}

impl ControlServer {
    /// Create and bind the control socket.
    pub fn bind(path: &Path) -> Result<Self, std::io::Error> {
        // Remove stale socket file
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;

        // Make socket world-readable so dynamodctl can connect
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660));
        }

        tracing::info!("control socket listening at {}", path.display());
        Ok(Self { listener })
    }

    /// Accept and handle any pending connections (non-blocking).
    /// Returns a list of actions that the main loop should execute.
    pub fn poll(&self, tree: &SupervisorTree) -> Vec<ControlAction> {
        let mut actions = Vec::new();

        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    match handle_connection(stream, tree) {
                        Ok(Some(action)) => actions.push(action),
                        Ok(None) => {}
                        Err(e) => {
                            tracing::debug!("control connection error: {e}");
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::warn!("control socket accept error: {e}");
                    break;
                }
            }
        }

        actions
    }
}

/// An action requested by a control client that the main loop must execute.
#[derive(Debug)]
pub enum ControlAction {
    StartService(String),
    StopService(String),
    RestartService(String),
    Shutdown(dynamod_common::protocol::ShutdownKind),
}

/// Handle a single control connection: read request, generate response, write response.
fn handle_connection(
    mut stream: UnixStream,
    tree: &SupervisorTree,
) -> Result<Option<ControlAction>, Box<dyn std::error::Error>> {
    stream.set_nonblocking(false)?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    // Read request
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(None);
    }

    let (msg, _) = protocol::decode(&buf[..n])?;
    let (response_body, action) = handle_request(msg.body, tree);

    // Send response
    let response = Message {
        id: msg.id,
        kind: MessageKind::Response {
            in_reply_to: msg.id,
        },
        body: response_body,
    };

    let data = protocol::encode(&response)?;
    stream.write_all(&data)?;

    Ok(action)
}

/// Process a request and return (response_body, optional_action).
fn handle_request(
    body: MessageBody,
    tree: &SupervisorTree,
) -> (MessageBody, Option<ControlAction>) {
    match body {
        MessageBody::StartService { name } => {
            if tree.get_worker(&name).is_some() {
                (MessageBody::Ack, Some(ControlAction::StartService(name)))
            } else {
                (
                    MessageBody::Error {
                        message: format!("unknown service: {name}"),
                    },
                    None,
                )
            }
        }
        MessageBody::StopService { name } => {
            if tree.get_worker(&name).is_some() {
                (MessageBody::Ack, Some(ControlAction::StopService(name)))
            } else {
                (
                    MessageBody::Error {
                        message: format!("unknown service: {name}"),
                    },
                    None,
                )
            }
        }
        MessageBody::RestartService { name } => {
            if tree.get_worker(&name).is_some() {
                (
                    MessageBody::Ack,
                    Some(ControlAction::RestartService(name)),
                )
            } else {
                (
                    MessageBody::Error {
                        message: format!("unknown service: {name}"),
                    },
                    None,
                )
            }
        }
        MessageBody::ServiceStatus { name } => match tree.get_worker(&name) {
            Some(w) => (
                MessageBody::ServiceInfo {
                    name: name.clone(),
                    status: w.state.display_name().to_string(),
                    pid: w.pid,
                    supervisor: w.parent.clone(),
                },
                None,
            ),
            None => (
                MessageBody::Error {
                    message: format!("unknown service: {name}"),
                },
                None,
            ),
        },
        MessageBody::ListServices => {
            let services: Vec<ServiceEntry> = tree
                .all_workers()
                .iter()
                .filter_map(|name| {
                    tree.get_worker(name).map(|w| ServiceEntry {
                        name: name.to_string(),
                        status: w.state.display_name().to_string(),
                        pid: w.pid,
                    })
                })
                .collect();
            (MessageBody::ServiceList { services }, None)
        }
        MessageBody::TreeStatus => {
            let text = format_tree(tree, tree.root_id(), 0);
            (MessageBody::Tree { text }, None)
        }
        MessageBody::Shutdown { kind } => {
            (MessageBody::Ack, Some(ControlAction::Shutdown(kind)))
        }
        _ => (
            MessageBody::Error {
                message: "unknown command".to_string(),
            },
            None,
        ),
    }
}

/// Format the supervisor tree as a text tree for display.
fn format_tree(tree: &SupervisorTree, node_id: &str, depth: usize) -> String {
    let mut output = String::new();
    let indent = "  ".repeat(depth);

    match tree.get(node_id) {
        Some(TreeNode::Supervisor(s)) => {
            output.push_str(&format!(
                "{indent}[supervisor: {node_id}] strategy={:?}\n",
                s.strategy
            ));
            for child_id in &s.children {
                output.push_str(&format_tree(tree, child_id, depth + 1));
            }
        }
        Some(TreeNode::Worker(w)) => {
            let pid_str = w
                .pid
                .map(|p| format!(" pid={p}"))
                .unwrap_or_default();
            output.push_str(&format!(
                "{indent}{node_id} [{}]{pid_str}\n",
                w.state.display_name()
            ));
        }
        None => {
            output.push_str(&format!("{indent}{node_id} [unknown]\n"));
        }
    }

    output
}
