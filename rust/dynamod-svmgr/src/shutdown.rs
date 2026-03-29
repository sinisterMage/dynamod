/// Graceful shutdown sequence for dynamod-svmgr.
///
/// Stops services in reverse dependency order, respects per-service
/// stop-signal and stop-timeout, escalates to SIGKILL after timeout.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

use crate::cgroup;
use crate::config::service::parse_duration_secs;
use crate::dependency::graph::DependencyGraph;
use crate::supervisor::tree::{SupervisorTree, TreeNode};
use crate::supervisor::lifecycle::ServiceState;

/// Execute a full graceful shutdown of all services.
///
/// 1. Compute reverse dependency order (dependents first, dependencies last)
/// 2. For each service: send stop signal, wait up to stop-timeout, then SIGKILL
/// 3. Clean up cgroups
/// 4. Return when all services have been stopped
pub fn execute_shutdown(
    tree: &mut SupervisorTree,
    dep_graph: &DependencyGraph,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    tracing::info!("beginning graceful shutdown sequence");

    // Compute reverse topological order using Kahn's algorithm
    let stop_order = compute_reverse_topo_order(dep_graph);

    tracing::info!(
        "stopping {} service(s) in reverse dependency order",
        stop_order.len()
    );

    for name in &stop_order {
        stop_service_graceful(tree, name, cgroup_hierarchy, cgroup_monitor);
    }

    // Final reap of any remaining zombies
    reap_remaining();

    tracing::info!("all services stopped");
}

/// Stop a single service gracefully:
/// 1. Send configured stop-signal (or SIGTERM)
/// 2. Wait up to stop-timeout
/// 3. If still alive, send SIGKILL and wait briefly
fn stop_service_graceful(
    tree: &mut SupervisorTree,
    name: &str,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    let (pid, stop_signal, stop_timeout) = match tree.get_worker(name) {
        Some(w) if w.pid.is_some() => {
            let sig = parse_signal(&w.def.shutdown.stop_signal);
            let timeout = parse_duration_secs(&w.def.shutdown.stop_timeout)
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(10));
            (w.pid.unwrap(), sig, timeout)
        }
        _ => return, // Not running or not found
    };

    tracing::info!(
        "stopping '{name}' (pid {pid}, signal={stop_signal}, timeout={}s)",
        stop_timeout.as_secs()
    );

    // Send the stop signal
    let nix_pid = Pid::from_raw(pid);
    if signal::kill(nix_pid, stop_signal).is_err() {
        tracing::debug!("kill({pid}, {stop_signal}) failed — process may already be dead");
    }

    if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
        w.state = ServiceState::Stopping {
            deadline: Some(Instant::now() + stop_timeout),
        };
    }

    // Wait for the process to exit (polling with short sleeps)
    let deadline = Instant::now() + stop_timeout;
    loop {
        match waitpid(nix_pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => {
                tracing::info!("'{name}' exited (code {code})");
                finalize_stop(tree, name, pid, cgroup_hierarchy, cgroup_monitor);
                return;
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                tracing::info!("'{name}' killed by signal {sig}");
                finalize_stop(tree, name, pid, cgroup_hierarchy, cgroup_monitor);
                return;
            }
            Ok(WaitStatus::StillAlive) => {}
            Err(nix::errno::Errno::ECHILD) => {
                // Already reaped
                finalize_stop(tree, name, pid, cgroup_hierarchy, cgroup_monitor);
                return;
            }
            _ => {}
        }

        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Timeout: escalate to SIGKILL
    tracing::warn!("'{name}' did not stop within timeout, sending SIGKILL");
    let _ = signal::kill(nix_pid, Signal::SIGKILL);

    // Wait briefly for SIGKILL to take effect
    let kill_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match waitpid(nix_pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {}
            _ => break,
        }
        if Instant::now() >= kill_deadline {
            tracing::error!("'{name}' (pid {pid}) did not die after SIGKILL");
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    finalize_stop(tree, name, pid, cgroup_hierarchy, cgroup_monitor);
}

/// Clean up after a stopped service.
fn finalize_stop(
    tree: &mut SupervisorTree,
    name: &str,
    pid: i32,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    tree.unregister_pid(pid);
    if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
        w.pid = None;
        w.state = ServiceState::Stopped;
    }

    // Clean up cgroup
    cgroup_monitor.unwatch(name);
    if let Some(hierarchy) = cgroup_hierarchy {
        let _ = hierarchy.remove_service_cgroup(name);
    }
}

/// Compute reverse topological order (dependents before dependencies).
/// Services with no dependents are stopped first; base services last.
fn compute_reverse_topo_order(graph: &DependencyGraph) -> Vec<String> {
    let all_services = graph.all_services();
    let mut in_degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut reverse_adj: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();

    // Build reverse graph: edges point from dependency -> dependent
    // in_degree counts how many services depend on each service
    for svc in all_services {
        in_degree.entry(svc.as_str()).or_insert(0);
        reverse_adj.entry(svc.as_str()).or_default();
        for dep in graph.dependencies_of(svc) {
            reverse_adj.entry(dep.as_str()).or_default().push(svc.as_str());
            *in_degree.entry(svc.as_str()).or_insert(0) += 1;
        }
    }

    // Kahn's algorithm on the reverse graph gives us: dependents first
    // Actually we want to stop dependents FIRST, so we do a normal topo sort
    // on the reverse graph.
    // "in_degree" here counts: for each service, how many things it depends ON.
    // Services with 0 in_degree depend on nothing -> they are "leaf" services.
    // We want to stop leaf-dependent services first, base services last.

    // Re-think: We want reverse dependency order.
    // Forward topo order: A, B, C where A has no deps, B depends on A, C depends on B
    // Reverse topo order for stopping: C, B, A (dependents first)

    // So we compute forward topo and reverse it.
    let mut forward_in_degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for svc in all_services {
        forward_in_degree.insert(svc.as_str(), graph.dependencies_of(svc).len());
    }

    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    for (svc, &deg) in &forward_in_degree {
        if deg == 0 {
            queue.push_back(svc);
        }
    }

    let mut forward_order: Vec<String> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();

    while let Some(svc) = queue.pop_front() {
        if !visited.insert(svc) {
            continue;
        }
        forward_order.push(svc.to_string());

        for dependent in graph.dependents_of(svc) {
            if let Some(deg) = forward_in_degree.get_mut(dependent.as_str()) {
                *deg = deg.saturating_sub(1);
                if *deg == 0 && !visited.contains(dependent.as_str()) {
                    queue.push_back(dependent.as_str());
                }
            }
        }
    }

    // Add any services not reached (cycles or isolated)
    for svc in all_services {
        if !visited.contains(svc.as_str()) {
            forward_order.push(svc.clone());
        }
    }

    // Reverse for shutdown order
    forward_order.reverse();
    forward_order
}

/// Parse a signal name (e.g. "SIGTERM", "SIGINT") into a nix Signal.
fn parse_signal(name: &str) -> Signal {
    match name {
        "SIGTERM" | "sigterm" => Signal::SIGTERM,
        "SIGINT" | "sigint" => Signal::SIGINT,
        "SIGQUIT" | "sigquit" => Signal::SIGQUIT,
        "SIGHUP" | "sighup" => Signal::SIGHUP,
        "SIGUSR1" | "sigusr1" => Signal::SIGUSR1,
        "SIGUSR2" | "sigusr2" => Signal::SIGUSR2,
        _ => Signal::SIGTERM,
    }
}

/// Reap any remaining zombie processes.
fn reap_remaining() {
    loop {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::ECHILD) => break,
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::service::ServiceDef;

    fn make_service(name: &str, requires: &[&str]) -> ServiceDef {
        let requires_str: Vec<String> = requires.iter().map(|s| s.to_string()).collect();
        let toml_str = format!(
            r#"
[service]
name = "{name}"
exec = ["/bin/true"]

[dependencies]
requires = {requires}

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "5s"
"#,
            requires = serde_json::to_string(&requires_str).unwrap(),
        );
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn test_reverse_topo_order_linear() {
        let defs = vec![
            make_service("a", &[]),
            make_service("b", &["a"]),
            make_service("c", &["b"]),
        ];
        let graph = DependencyGraph::build(&defs);
        let order = compute_reverse_topo_order(&graph);

        // c depends on b depends on a -> stop c first, then b, then a
        let pos_a = order.iter().position(|s| s == "a").unwrap();
        let pos_b = order.iter().position(|s| s == "b").unwrap();
        let pos_c = order.iter().position(|s| s == "c").unwrap();
        assert!(pos_c < pos_b, "c should stop before b");
        assert!(pos_b < pos_a, "b should stop before a");
    }

    #[test]
    fn test_reverse_topo_order_diamond() {
        let defs = vec![
            make_service("a", &[]),
            make_service("b", &["a"]),
            make_service("c", &["a"]),
            make_service("d", &["b", "c"]),
        ];
        let graph = DependencyGraph::build(&defs);
        let order = compute_reverse_topo_order(&graph);

        let pos_a = order.iter().position(|s| s == "a").unwrap();
        let pos_b = order.iter().position(|s| s == "b").unwrap();
        let pos_c = order.iter().position(|s| s == "c").unwrap();
        let pos_d = order.iter().position(|s| s == "d").unwrap();

        // d should stop first, a should stop last
        assert!(pos_d < pos_b, "d before b");
        assert!(pos_d < pos_c, "d before c");
        assert!(pos_b < pos_a, "b before a");
        assert!(pos_c < pos_a, "c before a");
    }

    #[test]
    fn test_reverse_topo_independent() {
        let defs = vec![
            make_service("x", &[]),
            make_service("y", &[]),
            make_service("z", &[]),
        ];
        let graph = DependencyGraph::build(&defs);
        let order = compute_reverse_topo_order(&graph);
        // All independent, any order is valid, just check all present
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn test_parse_signal_names() {
        assert_eq!(parse_signal("SIGTERM"), Signal::SIGTERM);
        assert_eq!(parse_signal("SIGINT"), Signal::SIGINT);
        assert_eq!(parse_signal("SIGHUP"), Signal::SIGHUP);
        assert_eq!(parse_signal("unknown"), Signal::SIGTERM);
    }
}
