/// Supervisor tree data structures.
///
/// A supervisor tree is a hierarchy where each node is either a Supervisor
/// (which manages children) or a Worker (an actual service process).
/// Supervisors can contain other supervisors, enabling nested supervision.
use std::collections::HashMap;

use crate::config::service::{RestartPolicy, ServiceDef};
use crate::config::supervisor::RestartStrategy;
use super::intensity::RestartIntensity;
use super::lifecycle::ServiceState;

/// Unique identifier for a node in the supervisor tree.
pub type NodeId = String;

/// A node in the supervisor tree.
#[derive(Debug)]
pub enum TreeNode {
    Supervisor(SupervisorNode),
    Worker(WorkerNode),
}

/// A supervisor node that manages child nodes.
#[derive(Debug)]
pub struct SupervisorNode {
    pub name: String,
    pub strategy: RestartStrategy,
    pub intensity: RestartIntensity,
    /// Children in start order. The order matters for OneForAll and RestForOne.
    pub children: Vec<NodeId>,
    /// Parent supervisor, if any (None for root).
    pub parent: Option<NodeId>,
}

/// A worker node representing a running service.
#[derive(Debug)]
pub struct WorkerNode {
    pub name: String,
    pub def: ServiceDef,
    pub state: ServiceState,
    pub restart_policy: RestartPolicy,
    /// Index within the parent supervisor's children list (for RestForOne ordering).
    pub start_index: usize,
    /// Parent supervisor.
    pub parent: NodeId,
    /// Current PID if running.
    pub pid: Option<i32>,
}

/// The complete supervisor tree.
#[derive(Debug)]
pub struct SupervisorTree {
    /// All nodes indexed by their ID.
    nodes: HashMap<NodeId, TreeNode>,
    /// The root supervisor ID.
    root_id: NodeId,
    /// Map from PID to worker node ID (for fast lookup on SIGCHLD).
    pid_map: HashMap<i32, NodeId>,
}

impl SupervisorTree {
    /// Create a new supervisor tree with a root supervisor.
    pub fn new(root_name: &str, strategy: RestartStrategy, intensity: RestartIntensity) -> Self {
        let root_id = root_name.to_string();
        let root = SupervisorNode {
            name: root_name.to_string(),
            strategy,
            intensity,
            children: Vec::new(),
            parent: None,
        };

        let mut nodes = HashMap::new();
        nodes.insert(root_id.clone(), TreeNode::Supervisor(root));

        Self {
            nodes,
            root_id,
            pid_map: HashMap::new(),
        }
    }

    /// Add a child supervisor under a parent supervisor.
    pub fn add_supervisor(
        &mut self,
        name: &str,
        parent_id: &str,
        strategy: RestartStrategy,
        intensity: RestartIntensity,
    ) -> Result<(), TreeError> {
        if self.nodes.contains_key(name) {
            return Err(TreeError::DuplicateNode(name.to_string()));
        }

        let node = SupervisorNode {
            name: name.to_string(),
            strategy,
            intensity,
            children: Vec::new(),
            parent: Some(parent_id.to_string()),
        };

        // Add to parent's children list
        match self.nodes.get_mut(parent_id) {
            Some(TreeNode::Supervisor(parent)) => {
                parent.children.push(name.to_string());
            }
            Some(TreeNode::Worker(_)) => {
                return Err(TreeError::ParentIsWorker(parent_id.to_string()));
            }
            None => {
                return Err(TreeError::ParentNotFound(parent_id.to_string()));
            }
        }

        self.nodes
            .insert(name.to_string(), TreeNode::Supervisor(node));
        Ok(())
    }

    /// Add a worker (service) under a supervisor.
    pub fn add_worker(
        &mut self,
        def: ServiceDef,
        parent_id: &str,
    ) -> Result<(), TreeError> {
        let name = def.service.name.clone();
        if self.nodes.contains_key(&name) {
            return Err(TreeError::DuplicateNode(name));
        }

        let start_index = match self.nodes.get_mut(parent_id) {
            Some(TreeNode::Supervisor(parent)) => {
                let idx = parent.children.len();
                parent.children.push(name.clone());
                idx
            }
            Some(TreeNode::Worker(_)) => {
                return Err(TreeError::ParentIsWorker(parent_id.to_string()));
            }
            None => {
                return Err(TreeError::ParentNotFound(parent_id.to_string()));
            }
        };

        let restart_policy = def.restart.policy.clone();
        let worker = WorkerNode {
            name: name.clone(),
            def,
            state: ServiceState::Stopped,
            restart_policy,
            start_index,
            parent: parent_id.to_string(),
            pid: None,
        };

        self.nodes.insert(name, TreeNode::Worker(worker));
        Ok(())
    }

    /// Register a PID for a worker (called after spawning).
    pub fn register_pid(&mut self, worker_id: &str, pid: i32) {
        if let Some(TreeNode::Worker(w)) = self.nodes.get_mut(worker_id) {
            w.pid = Some(pid);
            w.state = ServiceState::Running;
        }
        self.pid_map.insert(pid, worker_id.to_string());
    }

    /// Unregister a PID (called after process exits).
    pub fn unregister_pid(&mut self, pid: i32) -> Option<NodeId> {
        self.pid_map.remove(&pid)
    }

    /// Look up which worker a PID belongs to.
    pub fn worker_for_pid(&self, pid: i32) -> Option<&str> {
        self.pid_map.get(&pid).map(|s| s.as_str())
    }

    /// Get a reference to a node.
    pub fn get(&self, id: &str) -> Option<&TreeNode> {
        self.nodes.get(id)
    }

    /// Get a mutable reference to a node.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut TreeNode> {
        self.nodes.get_mut(id)
    }

    /// Get a worker node by name.
    pub fn get_worker(&self, id: &str) -> Option<&WorkerNode> {
        match self.nodes.get(id) {
            Some(TreeNode::Worker(w)) => Some(w),
            _ => None,
        }
    }

    /// Get a mutable worker node by name.
    pub fn get_worker_mut(&mut self, id: &str) -> Option<&mut WorkerNode> {
        match self.nodes.get_mut(id) {
            Some(TreeNode::Worker(w)) => Some(w),
            _ => None,
        }
    }

    /// Get a supervisor node by name.
    pub fn get_supervisor(&self, id: &str) -> Option<&SupervisorNode> {
        match self.nodes.get(id) {
            Some(TreeNode::Supervisor(s)) => Some(s),
            _ => None,
        }
    }

    /// Get the root supervisor ID.
    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    /// Get all worker IDs under a supervisor (direct children only).
    pub fn workers_under(&self, supervisor_id: &str) -> Vec<NodeId> {
        let Some(TreeNode::Supervisor(sup)) = self.nodes.get(supervisor_id) else {
            return Vec::new();
        };
        sup.children
            .iter()
            .filter(|id| matches!(self.nodes.get(*id), Some(TreeNode::Worker(_))))
            .cloned()
            .collect()
    }

    /// Get children of a supervisor in start order.
    pub fn children_of(&self, supervisor_id: &str) -> Vec<NodeId> {
        match self.nodes.get(supervisor_id) {
            Some(TreeNode::Supervisor(s)) => s.children.clone(),
            _ => Vec::new(),
        }
    }

    /// Get the parent supervisor of a node.
    pub fn parent_of(&self, node_id: &str) -> Option<&str> {
        match self.nodes.get(node_id) {
            Some(TreeNode::Worker(w)) => Some(&w.parent),
            Some(TreeNode::Supervisor(s)) => s.parent.as_deref(),
            None => None,
        }
    }

    /// Get all worker names in the tree.
    pub fn all_workers(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter_map(|(id, node)| match node {
                TreeNode::Worker(_) => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("duplicate node: '{0}'")]
    DuplicateNode(String),
    #[error("parent not found: '{0}'")]
    ParentNotFound(String),
    #[error("parent '{0}' is a worker, not a supervisor")]
    ParentIsWorker(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_test_tree() -> SupervisorTree {
        let intensity = RestartIntensity::new(5, Duration::from_secs(60));
        let mut tree = SupervisorTree::new("root", RestartStrategy::OneForOne, intensity.clone());

        tree.add_supervisor("network", "root", RestartStrategy::OneForAll, intensity.clone())
            .unwrap();
        tree.add_supervisor("data", "root", RestartStrategy::RestForOne, intensity)
            .unwrap();

        tree
    }

    #[test]
    fn test_tree_structure() {
        let tree = make_test_tree();
        assert_eq!(tree.root_id(), "root");
        assert!(tree.get_supervisor("root").is_some());
        assert!(tree.get_supervisor("network").is_some());
        assert!(tree.get_supervisor("data").is_some());
        assert_eq!(tree.children_of("root"), vec!["network", "data"]);
    }

    #[test]
    fn test_parent_lookup() {
        let tree = make_test_tree();
        assert_eq!(tree.parent_of("network"), Some("root"));
        assert_eq!(tree.parent_of("data"), Some("root"));
        assert_eq!(tree.parent_of("root"), None);
    }

    #[test]
    fn test_duplicate_node_rejected() {
        let mut tree = make_test_tree();
        let intensity = RestartIntensity::new(5, Duration::from_secs(60));
        let result = tree.add_supervisor("network", "root", RestartStrategy::OneForOne, intensity);
        assert!(result.is_err());
    }

    #[test]
    fn test_pid_registration() {
        let mut tree = make_test_tree();

        // We need a minimal ServiceDef for adding a worker
        let toml_str = r#"
[service]
name = "svc1"
exec = ["/bin/true"]
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "network").unwrap();

        tree.register_pid("svc1", 1234);
        assert_eq!(tree.worker_for_pid(1234), Some("svc1"));

        let removed = tree.unregister_pid(1234);
        assert_eq!(removed, Some("svc1".to_string()));
        assert_eq!(tree.worker_for_pid(1234), None);
    }
}
