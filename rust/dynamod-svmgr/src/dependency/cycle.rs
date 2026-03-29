/// Cycle detection for the dependency graph.
///
/// Uses DFS with three-color marking to detect cycles and report
/// the exact cycle path for diagnostics.
use std::collections::{HashMap, HashSet};

use super::graph::DependencyGraph;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    White, // Not visited
    Gray,  // In current DFS path (visiting)
    Black, // Fully processed
}

/// Detect cycles in the dependency graph.
/// Returns a list of cycles found (each cycle is a vector of service names).
pub fn detect_cycles(graph: &DependencyGraph) -> Vec<Vec<String>> {
    let services = graph.all_services();
    let mut colors: HashMap<&str, Color> = services.iter().map(|s| (s.as_str(), Color::White)).collect();
    let mut path: Vec<String> = Vec::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();

    for service in services {
        if colors[service.as_str()] == Color::White {
            dfs(graph, service, &mut colors, &mut path, &mut cycles);
        }
    }

    cycles
}

fn dfs<'a>(
    graph: &'a DependencyGraph,
    node: &'a str,
    colors: &mut HashMap<&'a str, Color>,
    path: &mut Vec<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    colors.insert(node, Color::Gray);
    path.push(node.to_string());

    for dep in graph.dependencies_of(node) {
        match colors.get(dep.as_str()) {
            Some(Color::Gray) => {
                // Found a cycle — extract it from the path
                if let Some(start) = path.iter().position(|s| s == dep) {
                    let cycle: Vec<String> = path[start..].to_vec();
                    cycles.push(cycle);
                }
            }
            Some(Color::White) | None => {
                dfs(graph, dep, colors, path, cycles);
            }
            Some(Color::Black) => {
                // Already fully processed, no cycle through here
            }
        }
    }

    path.pop();
    colors.insert(node, Color::Black);
}

/// Validate the dependency graph has no cycles. Returns Ok(()) or an error
/// with a human-readable description of all cycles found.
pub fn validate_no_cycles(graph: &DependencyGraph) -> Result<(), CycleError> {
    let cycles = detect_cycles(graph);
    if cycles.is_empty() {
        Ok(())
    } else {
        Err(CycleError { cycles })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("dependency cycles detected: {}", format_cycles(&self.cycles))]
pub struct CycleError {
    pub cycles: Vec<Vec<String>>,
}

fn format_cycles(cycles: &[Vec<String>]) -> String {
    cycles
        .iter()
        .map(|c| c.join(" -> ") + " -> " + &c[0])
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::service::ServiceDef;
    use crate::dependency::graph::DependencyGraph;

    fn make_service(name: &str, requires: &[&str]) -> ServiceDef {
        let requires_str: Vec<String> = requires.iter().map(|s| s.to_string()).collect();
        let toml_str = format!(
            r#"
[service]
name = "{name}"
exec = ["/bin/true"]

[dependencies]
requires = {requires}
"#,
            requires = serde_json::to_string(&requires_str).unwrap(),
        );
        toml::from_str(&toml_str).unwrap()
    }

    #[test]
    fn test_no_cycles() {
        let defs = vec![
            make_service("a", &[]),
            make_service("b", &["a"]),
            make_service("c", &["b"]),
        ];
        let graph = DependencyGraph::build(&defs);
        assert!(detect_cycles(&graph).is_empty());
        assert!(validate_no_cycles(&graph).is_ok());
    }

    #[test]
    fn test_simple_cycle() {
        let defs = vec![
            make_service("a", &["b"]),
            make_service("b", &["a"]),
        ];
        let graph = DependencyGraph::build(&defs);
        let cycles = detect_cycles(&graph);
        assert!(!cycles.is_empty());
        assert!(validate_no_cycles(&graph).is_err());
    }

    #[test]
    fn test_three_node_cycle() {
        let defs = vec![
            make_service("a", &["c"]),
            make_service("b", &["a"]),
            make_service("c", &["b"]),
        ];
        let graph = DependencyGraph::build(&defs);
        let cycles = detect_cycles(&graph);
        assert!(!cycles.is_empty());
    }

    #[test]
    fn test_diamond_no_cycle() {
        let defs = vec![
            make_service("a", &[]),
            make_service("b", &["a"]),
            make_service("c", &["a"]),
            make_service("d", &["b", "c"]),
        ];
        let graph = DependencyGraph::build(&defs);
        assert!(detect_cycles(&graph).is_empty());
    }
}
