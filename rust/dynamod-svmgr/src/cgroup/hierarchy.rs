/// Cgroup v2 hierarchy management.
///
/// Creates and manages the cgroup tree under /sys/fs/cgroup/dynamod/.
/// Each service gets its own cgroup for resource isolation.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The root of dynamod's cgroup hierarchy.
const CGROUP_ROOT: &str = "/sys/fs/cgroup/dynamod";

/// Controllers to enable in the subtree.
const CONTROLLERS: &[&str] = &["cpu", "memory", "io", "pids"];

/// Manages the cgroup hierarchy for dynamod services.
#[derive(Debug)]
pub struct CgroupHierarchy {
    root: PathBuf,
}

impl CgroupHierarchy {
    /// Initialize the cgroup hierarchy. Creates /sys/fs/cgroup/dynamod/
    /// and enables required controllers.
    pub fn init() -> Result<Self, CgroupError> {
        Self::init_at(Path::new(CGROUP_ROOT))
    }

    /// Initialize at a custom path (for testing).
    pub fn init_at(root: &Path) -> Result<Self, CgroupError> {
        // Create the root directory
        fs::create_dir_all(root).map_err(|e| CgroupError::CreateDir(root.display().to_string(), e))?;

        // Enable controllers in the parent's subtree_control
        if let Some(parent) = root.parent() {
            let subtree_control = parent.join("cgroup.subtree_control");
            if subtree_control.exists() {
                for controller in CONTROLLERS {
                    // Errors are non-fatal — the controller might not be available
                    let _ = fs::write(&subtree_control, format!("+{controller}"));
                }
            }
        }

        // Enable controllers in our own subtree_control
        let our_subtree = root.join("cgroup.subtree_control");
        for controller in CONTROLLERS {
            let _ = fs::write(&our_subtree, format!("+{controller}"));
        }

        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Create a cgroup for a service.
    /// Returns the path to the service's cgroup directory.
    pub fn create_service_cgroup(&self, service_name: &str) -> Result<PathBuf, CgroupError> {
        let path = self.root.join(service_name);
        fs::create_dir_all(&path)
            .map_err(|e| CgroupError::CreateDir(path.display().to_string(), e))?;

        // Enable controllers in this cgroup's subtree (for nested processes)
        let subtree = path.join("cgroup.subtree_control");
        for controller in CONTROLLERS {
            let _ = fs::write(&subtree, format!("+{controller}"));
        }

        Ok(path)
    }

    /// Remove a service's cgroup. On a real cgroup filesystem, the directory
    /// only contains virtual files. On a regular filesystem (e.g. in tests),
    /// we remove contents recursively.
    pub fn remove_service_cgroup(&self, service_name: &str) -> Result<(), CgroupError> {
        let path = self.root.join(service_name);
        if path.exists() {
            // Try rmdir first (works on real cgroups), fall back to recursive remove
            if fs::remove_dir(&path).is_err() {
                fs::remove_dir_all(&path)
                    .map_err(|e| CgroupError::RemoveDir(path.display().to_string(), e))?;
            }
        }
        Ok(())
    }

    /// Move a process into a service's cgroup.
    pub fn add_process(&self, service_name: &str, pid: u32) -> Result<(), CgroupError> {
        let procs_path = self.root.join(service_name).join("cgroup.procs");
        fs::write(&procs_path, pid.to_string())
            .map_err(|e| CgroupError::WriteFile(procs_path.display().to_string(), e))
    }

    /// Get the cgroup path for a service.
    pub fn service_path(&self, service_name: &str) -> PathBuf {
        self.root.join(service_name)
    }

    /// Check if the cgroup root exists (cgroups v2 is available).
    pub fn is_available() -> bool {
        Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
    }

    /// Get the root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for CgroupHierarchy {
    fn drop(&mut self) {
        // Best-effort cleanup of the root directory
        let _ = fs::remove_dir(&self.root);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CgroupError {
    #[error("failed to create directory {0}: {1}")]
    CreateDir(String, io::Error),
    #[error("failed to remove directory {0}: {1}")]
    RemoveDir(String, io::Error),
    #[error("failed to write {0}: {1}")]
    WriteFile(String, io::Error),
    #[error("failed to read {0}: {1}")]
    ReadFile(String, io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_hierarchy_creation() {
        let tmp = tempdir();
        let root = tmp.join("cgroup/dynamod");

        let hierarchy = CgroupHierarchy::init_at(&root).unwrap();
        assert!(root.exists());

        let svc_path = hierarchy.create_service_cgroup("test-svc").unwrap();
        assert!(svc_path.exists());
        assert_eq!(svc_path, root.join("test-svc"));

        hierarchy.remove_service_cgroup("test-svc").unwrap();
        assert!(!svc_path.exists());
    }

    #[test]
    fn test_service_path() {
        let tmp = tempdir();
        let root = tmp.join("cgroup/dynamod");
        let hierarchy = CgroupHierarchy::init_at(&root).unwrap();

        assert_eq!(
            hierarchy.service_path("nginx"),
            root.join("nginx")
        );
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dynamod-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
