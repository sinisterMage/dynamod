use std::collections::HashSet;

use super::service::{ReadinessType, ServiceDef};
use super::supervisor::SupervisorDef;

/// Validation errors for service and supervisor configurations.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("service '{0}': exec command is empty")]
    EmptyExec(String),
    #[error("service '{0}': readiness type 'tcp-port' requires a port number")]
    MissingPort(String),
    #[error("service '{0}': readiness type 'exec' requires a check-exec command")]
    MissingCheckExec(String),
    #[error("service '{0}': references unknown supervisor '{1}'")]
    UnknownSupervisor(String, String),
    #[error("service '{0}': dependency '{1}' references unknown service")]
    UnknownDependency(String, String),
    #[error("duplicate service name: '{0}'")]
    DuplicateService(String),
    #[error("duplicate supervisor name: '{0}'")]
    DuplicateSupervisor(String),
}

/// Validate a single service definition.
pub fn validate_service(def: &ServiceDef) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let name = &def.service.name;

    if def.service.exec.is_empty() {
        errors.push(ValidationError::EmptyExec(name.clone()));
    }

    if def.readiness.readiness_type == ReadinessType::TcpPort && def.readiness.port.is_none() {
        errors.push(ValidationError::MissingPort(name.clone()));
    }

    if def.readiness.readiness_type == ReadinessType::Exec && def.readiness.check_exec.is_none() {
        errors.push(ValidationError::MissingCheckExec(name.clone()));
    }

    errors
}

/// Validate all services and supervisors together (cross-references).
pub fn validate_all(
    services: &[ServiceDef],
    supervisors: &[SupervisorDef],
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Check for duplicate names
    let mut service_names = HashSet::new();
    for svc in services {
        if !service_names.insert(&svc.service.name) {
            errors.push(ValidationError::DuplicateService(svc.service.name.clone()));
        }
    }

    let mut supervisor_names: HashSet<&String> = HashSet::new();
    // "root" is always implicitly available
    let root = "root".to_string();
    supervisor_names.insert(&root);
    for sup in supervisors {
        if !supervisor_names.insert(&sup.supervisor.name) {
            errors.push(ValidationError::DuplicateSupervisor(
                sup.supervisor.name.clone(),
            ));
        }
    }

    // Validate each service
    for svc in services {
        errors.extend(validate_service(svc));

        // Check supervisor reference
        if !supervisor_names.contains(&svc.service.supervisor) {
            errors.push(ValidationError::UnknownSupervisor(
                svc.service.name.clone(),
                svc.service.supervisor.clone(),
            ));
        }

        // Check dependency references
        for dep in &svc.dependencies.requires {
            if !service_names.contains(dep) {
                errors.push(ValidationError::UnknownDependency(
                    svc.service.name.clone(),
                    dep.clone(),
                ));
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_empty_exec() {
        let toml_str = r#"
[service]
name = "bad"
exec = []
"#;
        let def: ServiceDef = toml::from_str(toml_str).unwrap();
        let errors = validate_service(&def);
        assert!(errors.iter().any(|e| matches!(e, ValidationError::EmptyExec(_))));
    }
}
