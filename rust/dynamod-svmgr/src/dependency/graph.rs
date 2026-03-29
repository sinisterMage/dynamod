/// Dependency graph for service startup ordering.
///
/// Each service declares:
/// - `requires`: hard dependencies (must be READY before this starts; blocks if dependency fails)
/// - `wants`: soft dependencies (ordering only if present; ignored if absent)
/// - `after`: ordering hints (start after, but no hard dependency)
/// - `before`: reverse ordering hints
use std::collections::{HashMap, HashSet};

use crate::config::service::ServiceDef;

/// A directed acyclic graph representing service dependencies.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// For each service: the set of services it depends on (edges from dependent -> dependency).
    /// These are the "requires" + resolved "after" + resolved "wants" edges.
    hard_deps: HashMap<String, HashSet<String>>,
    /// Just the hard (requires) edges — used to propagate failures.
    requires: HashMap<String, HashSet<String>>,
    /// Reverse map: for each service, the set of services that depend on it.
    reverse_deps: HashMap<String, HashSet<String>>,
    /// All known service names.
    services: HashSet<String>,
}

impl DependencyGraph {
    /// Build a dependency graph from a set of service definitions.
    pub fn build(defs: &[ServiceDef]) -> Self {
        let services: HashSet<String> = defs.iter().map(|d| d.service.name.clone()).collect();
        let mut hard_deps: HashMap<String, HashSet<String>> = HashMap::new();
        let mut requires: HashMap<String, HashSet<String>> = HashMap::new();
        let mut reverse_deps: HashMap<String, HashSet<String>> = HashMap::new();

        // Initialize all services
        for name in &services {
            hard_deps.insert(name.clone(), HashSet::new());
            requires.insert(name.clone(), HashSet::new());
            reverse_deps.insert(name.clone(), HashSet::new());
        }

        for def in defs {
            let name = &def.service.name;

            // `requires` — hard dependency + ordering
            for dep in &def.dependencies.requires {
                if services.contains(dep) {
                    hard_deps.get_mut(name).unwrap().insert(dep.clone());
                    requires.get_mut(name).unwrap().insert(dep.clone());
                    reverse_deps.get_mut(dep).unwrap().insert(name.clone());
                }
            }

            // `wants` — soft ordering (only if the wanted service exists)
            for dep in &def.dependencies.wants {
                if services.contains(dep) {
                    hard_deps.get_mut(name).unwrap().insert(dep.clone());
                    reverse_deps.get_mut(dep).unwrap().insert(name.clone());
                }
            }

            // `after` — ordering only (not a hard dep)
            for dep in &def.dependencies.after {
                if services.contains(dep) {
                    hard_deps.get_mut(name).unwrap().insert(dep.clone());
                    reverse_deps.get_mut(dep).unwrap().insert(name.clone());
                }
            }

            // `before` — reverse ordering (this service should start before those)
            for dep in &def.dependencies.before {
                if services.contains(dep) {
                    hard_deps.get_mut(dep).unwrap().insert(name.clone());
                    reverse_deps.get_mut(name).unwrap().insert(dep.clone());
                }
            }
        }

        Self {
            hard_deps,
            requires,
            reverse_deps,
            services,
        }
    }

    /// Get the set of all dependencies for a service (requires + wants + after).
    pub fn dependencies_of(&self, service: &str) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> =
            std::sync::LazyLock::new(HashSet::new);
        self.hard_deps.get(service).unwrap_or(&EMPTY)
    }

    /// Get the hard (requires) dependencies for a service.
    pub fn requires_of(&self, service: &str) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> =
            std::sync::LazyLock::new(HashSet::new);
        self.requires.get(service).unwrap_or(&EMPTY)
    }

    /// Get services that depend on the given service.
    pub fn dependents_of(&self, service: &str) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> =
            std::sync::LazyLock::new(HashSet::new);
        self.reverse_deps.get(service).unwrap_or(&EMPTY)
    }

    /// Get all service names.
    pub fn all_services(&self) -> &HashSet<String> {
        &self.services
    }

    /// Get the number of unmet dependencies for a service, given a set of ready services.
    pub fn unmet_count(&self, service: &str, ready: &HashSet<String>) -> usize {
        self.dependencies_of(service)
            .iter()
            .filter(|dep| !ready.contains(*dep))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_simple_dependency() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);

        assert!(graph.dependencies_of("b").contains("a"));
        assert!(graph.dependents_of("a").contains("b"));
        assert!(graph.dependencies_of("a").is_empty());
    }

    #[test]
    fn test_diamond_dependency() {
        // D depends on B and C; B and C both depend on A
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["a"], &[]),
            make_service("d", &["b", "c"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);

        assert_eq!(graph.dependencies_of("d").len(), 2);
        assert!(graph.dependencies_of("d").contains("b"));
        assert!(graph.dependencies_of("d").contains("c"));
        assert_eq!(graph.dependencies_of("a").len(), 0);
    }

    #[test]
    fn test_unmet_count() {
        let defs = vec![
            make_service("a", &[], &[]),
            make_service("b", &["a"], &[]),
            make_service("c", &["a", "b"], &[]),
        ];
        let graph = DependencyGraph::build(&defs);

        let empty: HashSet<String> = HashSet::new();
        assert_eq!(graph.unmet_count("a", &empty), 0);
        assert_eq!(graph.unmet_count("b", &empty), 1);
        assert_eq!(graph.unmet_count("c", &empty), 2);

        let mut ready = HashSet::new();
        ready.insert("a".to_string());
        assert_eq!(graph.unmet_count("b", &ready), 0);
        assert_eq!(graph.unmet_count("c", &ready), 1);
    }

    #[test]
    fn test_unknown_deps_ignored() {
        let defs = vec![make_service("a", &["nonexistent"], &[])];
        let graph = DependencyGraph::build(&defs);
        assert!(graph.dependencies_of("a").is_empty());
    }
}
