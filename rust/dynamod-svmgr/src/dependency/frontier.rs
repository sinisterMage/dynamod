/// Dynamic frontier algorithm for dependency-aware parallel service startup.
///
/// Instead of computing a single topological order, we maintain a frontier
/// of services whose dependencies are all satisfied. This maximizes
/// parallelism: services start as soon as their dependencies are ready.
///
/// Algorithm:
/// 1. Compute unmet[s] = count of unready dependencies for each service
/// 2. Frontier = { s : unmet[s] == 0 }
/// 3. When service becomes READY: decrement unmet for all dependents
/// 4. When service FAILS: block all transitive dependents via `requires`
use std::collections::{HashMap, HashSet, VecDeque};

use super::graph::DependencyGraph;

/// Tracks the startup frontier state.
#[derive(Debug)]
pub struct StartupFrontier {
    /// Number of unmet dependencies for each service.
    unmet: HashMap<String, usize>,
    /// Services that are ready to start (all deps satisfied).
    ready_to_start: VecDeque<String>,
    /// Services that have been started and are now ready.
    ready: HashSet<String>,
    /// Services that have been started but are not yet ready.
    pending: HashSet<String>,
    /// Services that are blocked (a required dependency failed).
    blocked: HashSet<String>,
    /// Services that have been completed (started and ready, or failed).
    completed: HashSet<String>,
}

impl StartupFrontier {
    /// Initialize the frontier from a dependency graph.
    pub fn new(graph: &DependencyGraph) -> Self {
        let mut unmet = HashMap::new();
        let mut ready_to_start = VecDeque::new();

        for service in graph.all_services() {
            let count = graph.dependencies_of(service).len();
            unmet.insert(service.clone(), count);
            if count == 0 {
                ready_to_start.push_back(service.clone());
            }
        }

        Self {
            unmet,
            ready_to_start,
            ready: HashSet::new(),
            pending: HashSet::new(),
            blocked: HashSet::new(),
            completed: HashSet::new(),
        }
    }

    /// Take the next batch of services that are ready to start.
    /// Returns an empty vec when no more services can start right now.
    pub fn take_ready(&mut self) -> Vec<String> {
        let batch: Vec<String> = self.ready_to_start.drain(..).collect();
        for s in &batch {
            self.pending.insert(s.clone());
        }
        batch
    }

    /// Mark a service as READY (started and confirmed operational).
    /// Updates the frontier: any dependents whose unmet count drops to 0
    /// become ready to start.
    pub fn mark_ready(&mut self, service: &str, graph: &DependencyGraph) {
        self.pending.remove(service);
        self.ready.insert(service.to_string());
        self.completed.insert(service.to_string());

        // Decrement unmet count for all dependents
        for dependent in graph.dependents_of(service) {
            if self.blocked.contains(dependent) {
                continue;
            }
            if let Some(count) = self.unmet.get_mut(dependent) {
                *count = count.saturating_sub(1);
                if *count == 0 && !self.completed.contains(dependent) && !self.pending.contains(dependent) {
                    self.ready_to_start.push_back(dependent.clone());
                }
            }
        }
    }

    /// Mark a service as FAILED.
    /// For `requires` dependencies: block all transitive dependents.
    /// For `after`-only dependencies: they can still start (ordering satisfied).
    pub fn mark_failed(&mut self, service: &str, graph: &DependencyGraph) {
        self.pending.remove(service);
        self.completed.insert(service.to_string());

        // Block all services that require this one (transitively)
        let mut to_block = VecDeque::new();
        for dependent in graph.dependents_of(service) {
            // Only block if this is a hard (requires) dependency
            if graph.requires_of(dependent).contains(service) {
                to_block.push_back(dependent.clone());
            } else {
                // Soft dependency: just decrement unmet and potentially unblock
                if let Some(count) = self.unmet.get_mut(dependent) {
                    *count = count.saturating_sub(1);
                    if *count == 0
                        && !self.completed.contains(dependent)
                        && !self.pending.contains(dependent)
                        && !self.blocked.contains(dependent)
                    {
                        self.ready_to_start.push_back(dependent.clone());
                    }
                }
            }
        }

        // BFS to transitively block all services that require the failed one
        while let Some(svc) = to_block.pop_front() {
            if self.blocked.contains(&svc) {
                continue;
            }
            self.blocked.insert(svc.clone());
            self.completed.insert(svc.clone());

            // Also block anything that requires this newly-blocked service
            for dep in graph.dependents_of(&svc) {
                if graph.requires_of(dep).contains(&svc) && !self.blocked.contains(dep) {
                    to_block.push_back(dep.clone());
                }
            }
        }
    }

    /// Check if all services have been processed.
    pub fn is_complete(&self) -> bool {
        self.completed.len() + self.blocked.len() >= self.unmet.len()
    }

    /// Check if there are services waiting to start.
    pub fn has_ready(&self) -> bool {
        !self.ready_to_start.is_empty()
    }

    /// Check if there are services currently pending (started but not ready).
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Get all blocked services.
    pub fn blocked_services(&self) -> &HashSet<String> {
        &self.blocked
    }

    /// Get all ready services.
    pub fn ready_services(&self) -> &HashSet<String> {
        &self.ready
    }

    /// Get the number of services still pending startup.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::service::ServiceDef;

    fn make_service(name: &str, requires: &[&str], after: &[&str]) -> ServiceDef {
        let requires_str: Vec<String> = requires.iter().map(|s| s.to_string()).collect();
        let after_str: Vec<String> = after.iter().map(|s| s.to_string()).collect();
        let toml_str = format!(
            r#"
[service]
name = "{name}"
exec = ["/bin/true"]

[dependencies]
requires = {requires}
after = {after}
"#,
            requires = serde_json::to_string(&requires_str).unwrap(),
            after = serde_json::to_string(&after_str).unwrap(),
        );
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn test_independent_services_all_ready() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &[], &[]),
            make_service("c", &[], &[]),
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        let batch = frontier.take_ready();
        assert_eq!(batch.len(), 3);
    }

    #[test]
    fn test_linear_chain() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["b"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        // Only "a" should be ready initially
        let batch1 = frontier.take_ready();
        assert_eq!(batch1, vec!["a"]);

        // Mark "a" ready -> "b" becomes available
        frontier.mark_ready("a", &graph);
        let batch2 = frontier.take_ready();
        assert_eq!(batch2, vec!["b"]);

        // Mark "b" ready -> "c" becomes available
        frontier.mark_ready("b", &graph);
        let batch3 = frontier.take_ready();
        assert_eq!(batch3, vec!["c"]);

        frontier.mark_ready("c", &graph);
        assert!(frontier.is_complete());
    }

    #[test]
    fn test_diamond_parallelism() {
        // A -> B, A -> C, B+C -> D
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["a"], &[]),
            make_service("d", &["b", "c"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        // Only "a" ready
        let batch1 = frontier.take_ready();
        assert_eq!(batch1, vec!["a"]);

        // "a" ready -> both "b" and "c" become available (parallel!)
        frontier.mark_ready("a", &graph);
        let mut batch2 = frontier.take_ready();
        batch2.sort();
        assert_eq!(batch2, vec!["b", "c"]);

        // "b" ready, but "d" still waits for "c"
        frontier.mark_ready("b", &graph);
        assert!(frontier.take_ready().is_empty());

        // "c" ready -> "d" becomes available
        frontier.mark_ready("c", &graph);
        let batch3 = frontier.take_ready();
        assert_eq!(batch3, vec!["d"]);

        frontier.mark_ready("d", &graph);
        assert!(frontier.is_complete());
    }

    #[test]
    fn test_failure_blocks_requires_deps() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["b"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        frontier.take_ready(); // "a"
        frontier.mark_failed("a", &graph);

        // Both "b" and "c" should be blocked (transitive)
        assert!(frontier.blocked_services().contains("b"));
        assert!(frontier.blocked_services().contains("c"));
        assert!(frontier.take_ready().is_empty());
    }

    #[test]
    fn test_failure_of_after_dep_does_not_block() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &[], &["a"]), // after, not requires
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        let batch1 = frontier.take_ready();
        assert_eq!(batch1, vec!["a"]);

        // "a" fails, but "b" only has an `after` dep, not `requires`
        frontier.mark_failed("a", &graph);
        let batch2 = frontier.take_ready();
        assert_eq!(batch2, vec!["b"]);
    }

    #[test]
    fn test_mixed_deps_partial_block() {
        // "d" requires "b" and is after "c"
        // If "b" fails, "d" is blocked
        // If "c" fails, "d" can still start (once "b" is ready)
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["a"], &[]),
            make_service("d", &["b"], &["c"]),
        ];
        let graph = DependencyGraph::build(&defs);
        let mut frontier = StartupFrontier::new(&graph);

        frontier.take_ready(); // "a"
        frontier.mark_ready("a", &graph);
        frontier.take_ready(); // "b", "c"

        // "c" fails (after-only dep for "d") — "d" unmet decrements but still waits for "b"
        frontier.mark_failed("c", &graph);
        assert!(!frontier.blocked_services().contains("d"));

        // "b" succeeds — "d" should now be ready
        frontier.mark_ready("b", &graph);
        let batch = frontier.take_ready();
        assert_eq!(batch, vec!["d"]);
    }
}
