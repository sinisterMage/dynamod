/// Linux namespace setup for service isolation.
///
/// Configures PID, mount, network, UTS, IPC, and user namespaces.
/// Called in the child process after fork(), before exec().
use std::path::Path;

use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};

use crate::config::service::NamespaceSection;

/// Errors that can occur during namespace setup.
#[derive(Debug, thiserror::Error)]
pub enum NamespaceError {
    #[error("unshare failed: {0}")]
    Unshare(nix::Error),
    #[error("mount failed for {0}: {1}")]
    Mount(String, nix::Error),
    #[error("mkdir failed for {0}: {1}")]
    Mkdir(String, std::io::Error),
}

/// Convert namespace string names to CloneFlags.
fn parse_namespace_flags(namespaces: &[String]) -> CloneFlags {
    let mut flags = CloneFlags::empty();
    for ns in namespaces {
        match ns.as_str() {
            "pid" => flags |= CloneFlags::CLONE_NEWPID,
            "mnt" | "mount" => flags |= CloneFlags::CLONE_NEWNS,
            "net" | "network" => flags |= CloneFlags::CLONE_NEWNET,
            "uts" => flags |= CloneFlags::CLONE_NEWUTS,
            "ipc" => flags |= CloneFlags::CLONE_NEWIPC,
            "user" => flags |= CloneFlags::CLONE_NEWUSER,
            "cgroup" => flags |= CloneFlags::CLONE_NEWCGROUP,
            other => {
                tracing::warn!("unknown namespace type: '{other}'");
            }
        }
    }
    flags
}

/// Apply namespace isolation in the child process.
/// Must be called after fork() and before exec().
pub fn apply_namespaces(config: &NamespaceSection) -> Result<(), NamespaceError> {
    let flags = parse_namespace_flags(&config.enable);

    if flags.is_empty() {
        return Ok(());
    }

    // Enter new namespaces
    unshare(flags).map_err(NamespaceError::Unshare)?;

    // If we entered a mount namespace, set up mount isolation
    if flags.contains(CloneFlags::CLONE_NEWNS) {
        setup_mount_namespace(config)?;
    }

    // If we entered a PID namespace, remount /proc
    if flags.contains(CloneFlags::CLONE_NEWPID) {
        // /proc remount happens only if we also have a mount namespace
        if flags.contains(CloneFlags::CLONE_NEWNS) {
            remount_proc()?;
        }
    }

    Ok(())
}

/// Set up mount namespace isolation.
fn setup_mount_namespace(config: &NamespaceSection) -> Result<(), NamespaceError> {
    // Make all mounts private (prevent propagation to host)
    mount::<str, str, str, str>(
        None,
        "/",
        None,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None,
    )
    .map_err(|e| NamespaceError::Mount("/".into(), e))?;

    // Read-only root filesystem
    if let Some(ref protect) = config.protect_system {
        match protect.as_str() {
            "strict" => {
                // Remount / as read-only
                mount::<str, str, str, str>(
                    None,
                    "/",
                    None,
                    MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_BIND,
                    None,
                )
                .map_err(|e| NamespaceError::Mount("/ (read-only)".into(), e))?;
            }
            "full" => {
                // Read-only /usr and /boot
                for target in &["/usr", "/boot"] {
                    if Path::new(target).exists() {
                        let _ = mount::<str, str, str, str>(
                            Some(target),
                            target,
                            None,
                            MsFlags::MS_BIND | MsFlags::MS_REC,
                            None,
                        );
                        let _ = mount::<str, str, str, str>(
                            None,
                            target,
                            None,
                            MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_BIND | MsFlags::MS_REC,
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    // Private /tmp
    if config.private_tmp {
        mount::<str, str, str, str>(
            Some("tmpfs"),
            "/tmp",
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some("mode=1777,size=128M"),
        )
        .map_err(|e| NamespaceError::Mount("/tmp".into(), e))?;
    }

    // Apply bind mounts
    for bm in &config.bind_mounts {
        // Ensure target exists
        if !Path::new(&bm.target).exists() {
            std::fs::create_dir_all(&bm.target)
                .map_err(|e| NamespaceError::Mkdir(bm.target.clone(), e))?;
        }

        // Bind mount
        mount::<str, str, str, str>(
            Some(bm.source.as_str()),
            bm.target.as_str(),
            None,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None,
        )
        .map_err(|e| NamespaceError::Mount(bm.target.clone(), e))?;

        // Make read-only if not writable
        if !bm.writable {
            mount::<str, str, str, str>(
                None,
                bm.target.as_str(),
                None,
                MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_BIND | MsFlags::MS_REC,
                None,
            )
            .map_err(|e| NamespaceError::Mount(format!("{} (readonly)", bm.target), e))?;
        }
    }

    Ok(())
}

/// Remount /proc for a new PID namespace.
fn remount_proc() -> Result<(), NamespaceError> {
    // Unmount the old /proc
    let _ = nix::mount::umount("/proc");

    // Mount new /proc for the PID namespace
    mount::<str, str, str, str>(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None,
    )
    .map_err(|e| NamespaceError::Mount("/proc".into(), e))
}

/// Build the set of namespace flags from config (for display/logging).
pub fn describe_namespaces(config: &NamespaceSection) -> String {
    if config.enable.is_empty() {
        return "none".to_string();
    }
    config.enable.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_namespace_flags() {
        let namespaces = vec!["pid".into(), "mnt".into(), "net".into()];
        let flags = parse_namespace_flags(&namespaces);
        assert!(flags.contains(CloneFlags::CLONE_NEWPID));
        assert!(flags.contains(CloneFlags::CLONE_NEWNS));
        assert!(flags.contains(CloneFlags::CLONE_NEWNET));
        assert!(!flags.contains(CloneFlags::CLONE_NEWUTS));
    }

    #[test]
    fn test_empty_namespaces() {
        let flags = parse_namespace_flags(&[]);
        assert!(flags.is_empty());
    }

    #[test]
    fn test_describe_namespaces() {
        let config = NamespaceSection {
            enable: vec!["pid".into(), "mnt".into()],
            bind_mounts: vec![],
            private_tmp: false,
            protect_system: None,
        };
        assert_eq!(describe_namespaces(&config), "pid, mnt");

        let empty = NamespaceSection {
            enable: vec![],
            bind_mounts: vec![],
            private_tmp: false,
            protect_system: None,
        };
        assert_eq!(describe_namespaces(&empty), "none");
    }
}
