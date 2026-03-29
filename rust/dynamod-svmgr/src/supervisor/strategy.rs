/// OTP-style restart strategies.
///
/// When a child process exits, the supervisor applies its strategy to determine
/// which children need to be restarted:
///
/// - **OneForOne**: Only restart the failed child.
/// - **OneForAll**: Stop all children, then restart all in start order.
/// - **RestForOne**: Stop the failed child and all children started after it,
///   then restart them in start order.
use crate::config::service::RestartPolicy;
use crate::config::supervisor::RestartStrategy;
use super::lifecycle::{ExitInfo, ServiceState};
use super::tree::{NodeId, SupervisorTree, TreeNode};

/// The result of applying a restart strategy.
#[derive(Debug, Clone)]
pub struct StrategyAction {
    /// Children to stop (in this order, typically reverse start order).
    pub to_stop: Vec<NodeId>,
    /// Children to start (in this order, typically start order).
    pub to_start: Vec<NodeId>,
    /// Whether the supervisor itself has failed (intensity exceeded).
    pub supervisor_failed: bool,
}

/// Determine whether a child should be restarted based on its restart policy
/// and exit info.
pub fn should_restart(policy: &RestartPolicy, exit: &ExitInfo) -> bool {
    match policy {
        RestartPolicy::Permanent => true,
        RestartPolicy::Transient => !exit.is_normal(),
        RestartPolicy::Temporary => false,
    }
}

/// Apply a restart strategy for a failed child within a supervisor.
///
/// Returns the list of children to stop and start.
/// The caller is responsible for actually stopping/starting them.
pub fn apply_strategy(
    tree: &mut SupervisorTree,
    supervisor_id: &str,
    failed_child_id: &str,
    exit_info: &ExitInfo,
) -> StrategyAction {
    // Get supervisor info
    let (strategy, children) = match tree.get(supervisor_id) {
        Some(TreeNode::Supervisor(s)) => (s.strategy.clone(), s.children.clone()),
        _ => {
            return StrategyAction {
                to_stop: vec![],
                to_start: vec![],
                supervisor_failed: false,
            }
        }
    };

    // Check if the failed child should be restarted
    let should_restart_child = match tree.get_worker(failed_child_id) {
        Some(w) => should_restart(&w.restart_policy, exit_info),
        None => false,
    };

    if !should_restart_child {
        // Mark as stopped/failed, no restart needed
        if let Some(TreeNode::Worker(w)) = tree.get_mut(failed_child_id) {
            w.state = if exit_info.is_normal() {
                ServiceState::Stopped
            } else {
                ServiceState::Failed {
                    exit_code: exit_info.exit_code,
                    signal: exit_info.signal,
                }
            };
        }
        return StrategyAction {
            to_stop: vec![],
            to_start: vec![],
            supervisor_failed: false,
        };
    }

    // Record restart in intensity tracker
    let intensity_exceeded = match tree.get_mut(supervisor_id) {
        Some(TreeNode::Supervisor(s)) => s.intensity.record_restart(),
        _ => false,
    };

    if intensity_exceeded {
        tracing::error!(
            "supervisor '{}': restart intensity exceeded, giving up",
            supervisor_id
        );
        // Mark all children as abandoned
        for child_id in &children {
            if let Some(TreeNode::Worker(w)) = tree.get_mut(child_id) {
                w.state = ServiceState::Abandoned;
            }
        }
        return StrategyAction {
            to_stop: children.clone(),
            to_start: vec![],
            supervisor_failed: true,
        };
    }

    match strategy {
        RestartStrategy::OneForOne => apply_one_for_one(failed_child_id),
        RestartStrategy::OneForAll => apply_one_for_all(&children, failed_child_id),
        RestartStrategy::RestForOne => apply_rest_for_one(&children, failed_child_id),
    }
}

/// OneForOne: Only restart the failed child.
fn apply_one_for_one(failed_child_id: &str) -> StrategyAction {
    StrategyAction {
        to_stop: vec![],
        to_start: vec![failed_child_id.to_string()],
        supervisor_failed: false,
    }
}

/// OneForAll: Stop all children (reverse start order), restart all (start order).
fn apply_one_for_all(children: &[NodeId], _failed_child_id: &str) -> StrategyAction {
    let mut to_stop: Vec<NodeId> = children.to_vec();
    to_stop.reverse(); // Stop in reverse start order

    let to_start: Vec<NodeId> = children.to_vec(); // Restart in start order

    StrategyAction {
        to_stop,
        to_start,
        supervisor_failed: false,
    }
}

/// RestForOne: Stop the failed child and all children started after it (reverse order),
/// then restart them in start order.
fn apply_rest_for_one(children: &[NodeId], failed_child_id: &str) -> StrategyAction {
    let failed_idx = children
        .iter()
        .position(|id| id == failed_child_id)
        .unwrap_or(0);

    // Children from the failed one onwards
    let affected: Vec<NodeId> = children[failed_idx..].to_vec();

    // Stop in reverse order
    let mut to_stop = affected.clone();
    to_stop.reverse();

    // Start in order
    let to_start = affected;

    StrategyAction {
        to_stop,
        to_start,
        supervisor_failed: false,
    }
}

/// Handle a supervisor failure by escalating to its parent.
/// Returns the grandparent supervisor ID and the action to take,
/// or None if this is the root supervisor.
pub fn escalate_failure(
    tree: &mut SupervisorTree,
    failed_supervisor_id: &str,
) -> Option<(NodeId, StrategyAction)> {
    let parent_id = tree.parent_of(failed_supervisor_id)?.to_string();

    let exit_info = ExitInfo {
        exit_code: None,
        signal: None,
    };

    let action = apply_strategy(tree, &parent_id, failed_supervisor_id, &exit_info);
    Some((parent_id, action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::intensity::RestartIntensity;
    use std::time::Duration;

    fn build_test_tree() -> SupervisorTree {
        let intensity = RestartIntensity::new(5, Duration::from_secs(60));

        let mut tree =
            SupervisorTree::new("root", RestartStrategy::OneForOne, intensity.clone());

        // Add a sub-supervisor with OneForAll strategy
        tree.add_supervisor(
            "group-all",
            "root",
            RestartStrategy::OneForAll,
            intensity.clone(),
        )
        .unwrap();

        // Add a sub-supervisor with RestForOne strategy
        tree.add_supervisor(
            "group-rest",
            "root",
            RestartStrategy::RestForOne,
            intensity,
        )
        .unwrap();

        // Add workers to group-all
        for name in ["svc-a", "svc-b", "svc-c"] {
            let toml_str = format!(
                r#"
[service]
name = "{name}"
exec = ["/bin/true"]
supervisor = "group-all"

[restart]
policy = "permanent"
"#
            );
            let def: crate::config::service::ServiceDef = toml::from_str(&toml_str).unwrap();
            tree.add_worker(def, "group-all").unwrap();
        }

        // Add workers to group-rest
        for name in ["svc-x", "svc-y", "svc-z"] {
            let toml_str = format!(
                r#"
[service]
name = "{name}"
exec = ["/bin/true"]
supervisor = "group-rest"

[restart]
policy = "permanent"
"#
            );
            let def: crate::config::service::ServiceDef = toml::from_str(&toml_str).unwrap();
            tree.add_worker(def, "group-rest").unwrap();
        }

        // Mark all as running
        for name in ["svc-a", "svc-b", "svc-c", "svc-x", "svc-y", "svc-z"] {
            if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
                w.state = ServiceState::Running;
            }
        }

        tree
    }

    #[test]
    fn test_one_for_one() {
        let mut tree = build_test_tree();

        // Simulate svc-b failing under the root supervisor (which is one-for-one)
        // But svc-b is under group-all, so let's test via root first
        // Actually, let's add a worker directly under root
        let toml_str = r#"
[service]
name = "direct"
exec = ["/bin/true"]

[restart]
policy = "permanent"
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "root").unwrap();

        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "root", "direct", &exit);

        assert!(!action.supervisor_failed);
        assert!(action.to_stop.is_empty());
        assert_eq!(action.to_start, vec!["direct"]);
    }

    #[test]
    fn test_one_for_all() {
        let mut tree = build_test_tree();

        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "group-all", "svc-b", &exit);

        assert!(!action.supervisor_failed);
        // Should stop all in reverse order
        assert_eq!(action.to_stop, vec!["svc-c", "svc-b", "svc-a"]);
        // Should restart all in start order
        assert_eq!(action.to_start, vec!["svc-a", "svc-b", "svc-c"]);
    }

    #[test]
    fn test_rest_for_one() {
        let mut tree = build_test_tree();

        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "group-rest", "svc-y", &exit);

        assert!(!action.supervisor_failed);
        // Should stop svc-y and svc-z (started after svc-y) in reverse order
        assert_eq!(action.to_stop, vec!["svc-z", "svc-y"]);
        // Should restart svc-y and svc-z in order
        assert_eq!(action.to_start, vec!["svc-y", "svc-z"]);
    }

    #[test]
    fn test_temporary_not_restarted() {
        let mut tree = build_test_tree();

        let toml_str = r#"
[service]
name = "temp-svc"
exec = ["/bin/true"]

[restart]
policy = "temporary"
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "root").unwrap();

        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "root", "temp-svc", &exit);

        assert!(action.to_start.is_empty());
        assert!(action.to_stop.is_empty());
    }

    #[test]
    fn test_transient_normal_exit_not_restarted() {
        let mut tree = build_test_tree();

        let toml_str = r#"
[service]
name = "trans-svc"
exec = ["/bin/true"]

[restart]
policy = "transient"
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "root").unwrap();

        // Normal exit (code 0) -> should not restart
        let exit = ExitInfo {
            exit_code: Some(0),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "root", "trans-svc", &exit);
        assert!(action.to_start.is_empty());
    }

    #[test]
    fn test_transient_failure_restarted() {
        let mut tree = build_test_tree();

        let toml_str = r#"
[service]
name = "trans-svc2"
exec = ["/bin/true"]

[restart]
policy = "transient"
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "root").unwrap();

        // Abnormal exit (code 1) -> should restart
        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };
        let action = apply_strategy(&mut tree, "root", "trans-svc2", &exit);
        assert_eq!(action.to_start, vec!["trans-svc2"]);
    }

    #[test]
    fn test_intensity_exceeded_marks_abandoned() {
        // Create a supervisor with very low intensity (max 2 restarts in 60s)
        let intensity = RestartIntensity::new(2, Duration::from_secs(60));
        let mut tree =
            SupervisorTree::new("root", RestartStrategy::OneForOne, intensity);

        let toml_str = r#"
[service]
name = "crasher"
exec = ["/bin/false"]

[restart]
policy = "permanent"
"#;
        let def: crate::config::service::ServiceDef = toml::from_str(toml_str).unwrap();
        tree.add_worker(def, "root").unwrap();

        let exit = ExitInfo {
            exit_code: Some(1),
            signal: None,
        };

        // First 2 restarts should succeed
        let action = apply_strategy(&mut tree, "root", "crasher", &exit);
        assert!(!action.supervisor_failed);
        assert_eq!(action.to_start, vec!["crasher"]);

        let action = apply_strategy(&mut tree, "root", "crasher", &exit);
        assert!(!action.supervisor_failed);
        assert_eq!(action.to_start, vec!["crasher"]);

        // 3rd restart should exceed intensity
        let action = apply_strategy(&mut tree, "root", "crasher", &exit);
        assert!(action.supervisor_failed);
        assert!(action.to_start.is_empty());
    }
}
