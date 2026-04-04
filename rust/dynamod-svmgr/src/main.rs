/// dynamod-svmgr: Service manager for the dynamod init system.
///
/// Phase 7: Graceful shutdown + hardening.
mod cgroup;
mod config;
mod dependency;
mod ipc;
mod namespace;
mod process;
mod shutdown;
mod supervisor;

use std::path::Path;
use std::time::{Duration, Instant};

use config::service::{self, parse_duration_secs, ServiceType};
use config::supervisor as sup_config;
use config::validate;
use ipc::init_channel::InitChannel;
use process::spawn;
use supervisor::intensity::RestartIntensity;
use supervisor::lifecycle::{ExitInfo, ServiceState};
use supervisor::strategy;
use supervisor::tree::{SupervisorTree, TreeNode};

use dynamod_common::protocol::MessageBody;

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("dynamod-svmgr starting");

    // Connect to init via IPC
    let mut init_channel = InitChannel::from_env();
    if init_channel.is_some() {
        tracing::info!("connected to dynamod-init via IPC");
    } else {
        tracing::warn!("no init connection, running standalone");
    }

    if let Some(ref mut ch) = init_channel {
        let _ = ch.send_heartbeat();
    }

    // Load configuration
    let services_dir = Path::new(dynamod_common::paths::SERVICES_DIR);
    let supervisors_dir = Path::new(dynamod_common::paths::SUPERVISORS_DIR);

    let services = service::load_services_dir(services_dir).unwrap_or_else(|e| {
        tracing::warn!("failed to load services: {e}");
        Vec::new()
    });
    tracing::info!("loaded {} service definition(s)", services.len());

    let supervisor_defs = sup_config::load_supervisors_dir(supervisors_dir).unwrap_or_else(|e| {
        tracing::warn!("failed to load supervisors: {e}");
        Vec::new()
    });
    tracing::info!("loaded {} supervisor definition(s)", supervisor_defs.len());

    let errors = validate::validate_all(&services, &supervisor_defs);
    for err in &errors {
        tracing::error!("config validation: {err}");
    }

    // Initialize cgroup hierarchy
    let cgroup_hierarchy = if cgroup::hierarchy::CgroupHierarchy::is_available() {
        match cgroup::hierarchy::CgroupHierarchy::init() {
            Ok(h) => {
                tracing::info!("cgroup v2 hierarchy initialized at {}", h.root().display());
                Some(h)
            }
            Err(e) => {
                tracing::warn!("failed to initialize cgroup hierarchy: {e}");
                None
            }
        }
    } else {
        tracing::info!("cgroup v2 not available, skipping resource isolation");
        None
    };

    let mut cgroup_monitor = cgroup::monitor::CgroupMonitor::new();

    // Start control socket server
    let control_server = match ipc::control::ControlServer::bind(
        Path::new(dynamod_common::paths::CONTROL_SOCK),
    ) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("failed to start control socket: {e}");
            None
        }
    };

    // Build supervisor tree
    let root_intensity = RestartIntensity::new(20, Duration::from_secs(600));
    let mut tree = SupervisorTree::new(
        "root",
        sup_config::RestartStrategy::OneForOne,
        root_intensity,
    );

    // Add configured supervisors
    for sup_def in &supervisor_defs {
        if sup_def.supervisor.name == "root" {
            // Update root's strategy from config
            if let Some(TreeNode::Supervisor(root)) = tree.get_mut("root") {
                root.strategy = sup_def.supervisor.strategy.clone();
                let window = parse_duration_secs(&sup_def.restart.max_restart_window)
                    .unwrap_or(600);
                root.intensity =
                    RestartIntensity::new(sup_def.restart.max_restarts, Duration::from_secs(window));
            }
            continue;
        }

        let parent = sup_def
            .supervisor
            .parent
            .as_deref()
            .unwrap_or("root");
        let window =
            parse_duration_secs(&sup_def.restart.max_restart_window).unwrap_or(300);
        let intensity =
            RestartIntensity::new(sup_def.restart.max_restarts, Duration::from_secs(window));

        if let Err(e) = tree.add_supervisor(
            &sup_def.supervisor.name,
            parent,
            sup_def.supervisor.strategy.clone(),
            intensity,
        ) {
            tracing::error!("failed to add supervisor '{}': {e}", sup_def.supervisor.name);
        }
    }

    // Add workers (services) to the tree
    for def in &services {
        let parent = &def.service.supervisor;
        // Ensure the parent supervisor exists; if not, fall back to root
        let parent_id = if tree.get(parent).is_some() {
            parent.as_str()
        } else {
            tracing::warn!(
                "service '{}' references unknown supervisor '{}', using root",
                def.service.name,
                parent
            );
            "root"
        };

        if let Err(e) = tree.add_worker(def.clone(), parent_id) {
            tracing::error!("failed to add service '{}' to tree: {e}", def.service.name);
        }
    }

    // Build dependency graph and validate no cycles
    let dep_graph = dependency::graph::DependencyGraph::build(&services);
    if let Err(e) = dependency::cycle::validate_no_cycles(&dep_graph) {
        tracing::error!("FATAL: {e}");
        tracing::error!("refusing to boot with cyclic dependencies");
        std::process::exit(1);
    }

    // Start services using the dynamic frontier algorithm
    let mut frontier = dependency::frontier::StartupFrontier::new(&dep_graph);
    let mut readiness_trackers: std::collections::HashMap<String, process::readiness::ReadinessTracker> =
        std::collections::HashMap::new();
    // Oneshot services waiting for process exit before being marked ready
    let mut pending_oneshots: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    tracing::info!("starting services via dependency frontier");

    // Process the frontier until all services are started
    loop {

        // Reap exited children during startup (handles oneshot completion)
        reap_oneshots_during_startup(
            &mut tree, &mut pending_oneshots, &mut frontier,
            &dep_graph, &cgroup_hierarchy, &mut cgroup_monitor,
        );

        // Start any services whose dependencies are satisfied
        let batch = frontier.take_ready();
        for name in &batch {
            let ready_fd = start_worker(&mut tree, name, &cgroup_hierarchy, &mut cgroup_monitor);

            // Set up readiness tracking
            if let Some(w) = tree.get_worker(name) {
                let is_oneshot = w.def.service.service_type == ServiceType::Oneshot;
                let mut tracker =
                    process::readiness::ReadinessTracker::new(&w.def.readiness, name);

                // Wire in the ready_fd if present
                if let Some(fd) = ready_fd {
                    tracker.set_ready_fd(fd);
                }

                if is_oneshot && tracker.is_immediate() {
                    // Oneshot with no readiness check: wait for process exit
                    pending_oneshots.insert(name.clone());
                    tracing::info!("service '{name}' is oneshot, waiting for exit");
                } else if tracker.is_immediate() {
                    // Long-running service with no readiness check: ready immediately
                    frontier.mark_ready(name, &dep_graph);
                    tracing::info!("service '{name}' ready (immediate)");
                } else {
                    readiness_trackers.insert(name.clone(), tracker);
                }
            }
        }

        // Poll pending readiness checks
        let tracker_names: Vec<String> = readiness_trackers.keys().cloned().collect();
        for name in tracker_names {
            let result = readiness_trackers[&name].check();
            match result {
                process::readiness::ReadinessResult::Ready => {
                    tracing::info!("service '{name}' ready");
                    readiness_trackers.remove(&name);
                    frontier.mark_ready(&name, &dep_graph);
                }
                process::readiness::ReadinessResult::TimedOut => {
                    tracing::error!("service '{name}' readiness timed out");
                    readiness_trackers.remove(&name);
                    frontier.mark_failed(&name, &dep_graph);
                }
                process::readiness::ReadinessResult::Failed(ref msg) => {
                    tracing::error!("service '{name}' readiness failed: {msg}");
                    readiness_trackers.remove(&name);
                    frontier.mark_failed(&name, &dep_graph);
                }
                process::readiness::ReadinessResult::NotReady => {}
            }
        }

        // If no more work to do, break out of startup
        if !frontier.has_ready() && readiness_trackers.is_empty() && pending_oneshots.is_empty() {
            break;
        }

        // Accept dynamodctl connections during startup (listener is non-blocking).
        if let Some(ref server) = control_server {
            dispatch_control_actions(
                server.poll(&tree),
                &mut tree,
                &cgroup_hierarchy,
                &mut cgroup_monitor,
                &mut init_channel,
                &dep_graph,
            );
        }

        // Brief sleep to avoid busy-polling readiness checks
        std::thread::sleep(Duration::from_millis(100));
    }

    let blocked = frontier.blocked_services();
    if !blocked.is_empty() {
        tracing::warn!(
            "{} service(s) blocked due to failed dependencies: {:?}",
            blocked.len(),
            blocked
        );
    }

    tracing::info!(
        "startup complete: {} ready, {} blocked",
        frontier.ready_services().len(),
        blocked.len(),
    );

    // Main event loop
    let heartbeat_interval = Duration::from_secs(5);
    let mut last_heartbeat = Instant::now();
    let mut heartbeat_failures: u32 = 0;
    const MAX_HEARTBEAT_FAILURES: u32 = 3;

    loop {
        // Reap exited children and apply supervisor strategies
        reap_and_handle_exits(&mut tree, &cgroup_hierarchy, &mut cgroup_monitor);

        // Handle control socket requests
        if let Some(ref server) = control_server {
            dispatch_control_actions(
                server.poll(&tree),
                &mut tree,
                &cgroup_hierarchy,
                &mut cgroup_monitor,
                &mut init_channel,
                &dep_graph,
            );
        }

        // Check for messages from init
        if let Some(ref mut ch) = init_channel {
            while let Some(msg) = ch.try_recv() {
                match msg.body {
                    MessageBody::ShutdownSignal { ref signal } => {
                        tracing::info!("shutdown signal from init: {signal}");
                        shutdown::execute_shutdown(
                            &mut tree, &dep_graph,
                            &cgroup_hierarchy, &mut cgroup_monitor,
                        );
                        std::process::exit(0);
                    }
                    MessageBody::HeartbeatAck => {}
                    _ => {
                        tracing::debug!("message from init: {:?}", msg.body);
                    }
                }
            }
        }

        // Poll cgroup events (OOM, memory pressure)
        for event in cgroup_monitor.poll() {
            match event {
                cgroup::monitor::CgroupEvent::OomKill { ref service_name, count } => {
                    tracing::error!(
                        "OOM kill in service '{service_name}' ({count} kill(s))"
                    );
                }
                cgroup::monitor::CgroupEvent::MemoryHigh {
                    ref service_name,
                    current_bytes,
                } => {
                    tracing::warn!(
                        "memory pressure in '{service_name}': {} bytes",
                        current_bytes
                    );
                }
            }
        }

        // Periodic heartbeat
        if last_heartbeat.elapsed() >= heartbeat_interval {
            if let Some(ref mut ch) = init_channel {
                if let Err(e) = ch.send_heartbeat() {
                    heartbeat_failures += 1;
                    tracing::error!(
                        "heartbeat failed ({heartbeat_failures}/{MAX_HEARTBEAT_FAILURES}): {e}"
                    );
                    if heartbeat_failures >= MAX_HEARTBEAT_FAILURES {
                        tracing::error!("init connection lost, exiting");
                        break;
                    }
                } else {
                    heartbeat_failures = 0;
                }
            }
            last_heartbeat = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn dispatch_control_actions(
    actions: Vec<ipc::control::ControlAction>,
    tree: &mut SupervisorTree,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
    init_channel: &mut Option<InitChannel>,
    dep_graph: &dependency::graph::DependencyGraph,
) {
    for action in actions {
        match action {
            ipc::control::ControlAction::StartService(name) => {
                start_worker(tree, &name, cgroup_hierarchy, cgroup_monitor);
            }
            ipc::control::ControlAction::StopService(name) => {
                stop_worker(tree, &name);
            }
            ipc::control::ControlAction::RestartService(name) => {
                let old_pid = tree.get_worker(&name).and_then(|w| w.pid);
                stop_worker(tree, &name);

                if let Some(pid) = old_pid {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        reap_and_handle_exits(tree, cgroup_hierarchy, cgroup_monitor);
                        let still_running = tree
                            .get_worker(&name)
                            .is_some_and(|w| w.pid == Some(pid));
                        if !still_running || Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }

                start_worker(tree, &name, cgroup_hierarchy, cgroup_monitor);
            }
            ipc::control::ControlAction::Reload => {
                tracing::info!("reload requested via control socket");
                reload_services(tree, cgroup_hierarchy, cgroup_monitor);
            }
            ipc::control::ControlAction::Shutdown(kind) => {
                tracing::info!("shutdown requested via control socket");
                shutdown::execute_shutdown(
                    tree,
                    dep_graph,
                    cgroup_hierarchy,
                    cgroup_monitor,
                );
                if let Some(ch) = init_channel {
                    let _ = ch.request_shutdown(kind);
                }
                std::process::exit(0);
            }
        }
    }
}

/// Start a worker process and register it in the tree.
/// Returns the ready_fd (read end) if the service uses fd-based readiness.
fn start_worker(
    tree: &mut SupervisorTree,
    worker_id: &str,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) -> Option<std::os::unix::io::RawFd> {
    let def = match tree.get_worker(worker_id) {
        Some(w) => w.def.clone(),
        None => return None,
    };

    // Log namespace info if configured
    if let Some(ref ns) = def.namespace {
        tracing::info!(
            "starting service '{}' (namespaces: {})",
            worker_id,
            namespace::setup::describe_namespaces(ns)
        );
    } else {
        tracing::info!("starting service '{}'", worker_id);
    }

    // Set up cgroup before spawning
    let cgroup_path = if let (Some(hierarchy), Some(cg_config)) =
        (cgroup_hierarchy, &def.cgroup)
    {
        match hierarchy.create_service_cgroup(worker_id) {
            Ok(path) => {
                if let Err(e) = cgroup::limits::apply_limits(&path, cg_config) {
                    tracing::warn!("failed to apply cgroup limits for '{worker_id}': {e}");
                }
                Some(path)
            }
            Err(e) => {
                tracing::warn!("failed to create cgroup for '{worker_id}': {e}");
                None
            }
        }
    } else {
        None
    };

    match spawn::spawn_service(&def) {
        Ok(spawned) => {
            let pid = spawned.pid.as_raw();
            let ready_fd = spawned.ready_fd;
            tree.register_pid(worker_id, pid);
            if let Some(TreeNode::Worker(w)) = tree.get_mut(worker_id) {
                w.state = ServiceState::Running;
            }

            // Move the process into its cgroup
            if let (Some(hierarchy), Some(_)) = (cgroup_hierarchy, &cgroup_path) {
                if let Err(e) = hierarchy.add_process(worker_id, pid as u32) {
                    tracing::warn!("failed to add pid {pid} to cgroup for '{worker_id}': {e}");
                }
                // Start monitoring the cgroup
                cgroup_monitor.watch(worker_id, &hierarchy.service_path(worker_id));
            }

            ready_fd
        }
        Err(e) => {
            tracing::error!("failed to start '{}': {e}", worker_id);
            if let Some(TreeNode::Worker(w)) = tree.get_mut(worker_id) {
                w.state = ServiceState::Failed {
                    exit_code: None,
                    signal: None,
                };
            }
            // Clean up cgroup on failure
            if let Some(hierarchy) = cgroup_hierarchy {
                let _ = hierarchy.remove_service_cgroup(worker_id);
            }
            None
        }
    }
}

/// Stop a worker process (send SIGTERM, mark as stopping).
fn stop_worker(tree: &mut SupervisorTree, worker_id: &str) {
    let pid = match tree.get_worker(worker_id) {
        Some(w) => w.pid,
        None => return,
    };

    if let Some(pid) = pid {
        tracing::info!("stopping service '{worker_id}' (pid {pid})");
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGTERM,
        );
        if let Some(TreeNode::Worker(w)) = tree.get_mut(worker_id) {
            w.state = ServiceState::Stopping { deadline: None };
        }
    }
}

/// Reap exited children during the startup phase, handling oneshot completion.
/// Oneshot services that exit with code 0 are marked ready in the frontier;
/// those that exit with nonzero are marked failed.
fn reap_oneshots_during_startup(
    tree: &mut SupervisorTree,
    pending_oneshots: &mut std::collections::HashSet<String>,
    frontier: &mut dependency::frontier::StartupFrontier,
    dep_graph: &dependency::graph::DependencyGraph,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};

    loop {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, code)) => {
                let worker_id = tree.unregister_pid(pid.as_raw());
                if let Some(ref name) = worker_id {
                    if pending_oneshots.remove(name.as_str()) {
                        // This is a pending oneshot — mark ready or failed
                        if code == 0 {
                            tracing::info!("oneshot '{name}' completed (code 0)");
                            if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
                                w.pid = None;
                                w.state = ServiceState::Stopped;
                            }
                            cgroup_monitor.unwatch(name);
                            if let Some(hierarchy) = cgroup_hierarchy {
                                let _ = hierarchy.remove_service_cgroup(name);
                            }
                            frontier.mark_ready(name, dep_graph);
                        } else {
                            tracing::error!("oneshot '{name}' failed (code {code})");
                            if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
                                w.pid = None;
                                w.state = ServiceState::Failed {
                                    exit_code: Some(code),
                                    signal: None,
                                };
                            }
                            cgroup_monitor.unwatch(name);
                            if let Some(hierarchy) = cgroup_hierarchy {
                                let _ = hierarchy.remove_service_cgroup(name);
                            }
                            frontier.mark_failed(name, dep_graph);
                        }
                        continue;
                    }
                }
                // Not a pending oneshot — handle normally
                if let Some(name) = worker_id {
                    // Re-register PID temporarily so handle_child_exit can unregister it
                    tree.register_pid(&name, pid.as_raw());
                    handle_child_exit(tree, pid.as_raw(), ExitInfo {
                        exit_code: Some(code),
                        signal: None,
                    }, cgroup_hierarchy, cgroup_monitor);
                }
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                let worker_id = tree.unregister_pid(pid.as_raw());
                if let Some(ref name) = worker_id {
                    if pending_oneshots.remove(name.as_str()) {
                        tracing::error!("oneshot '{name}' killed (signal {sig})");
                        if let Some(TreeNode::Worker(w)) = tree.get_mut(name) {
                            w.pid = None;
                            w.state = ServiceState::Failed {
                                exit_code: None,
                                signal: Some(sig as i32),
                            };
                        }
                        cgroup_monitor.unwatch(name);
                        if let Some(hierarchy) = cgroup_hierarchy {
                            let _ = hierarchy.remove_service_cgroup(name);
                        }
                        frontier.mark_failed(name, dep_graph);
                        continue;
                    }
                }
                if let Some(name) = worker_id {
                    tree.register_pid(&name, pid.as_raw());
                    handle_child_exit(tree, pid.as_raw(), ExitInfo {
                        exit_code: None,
                        signal: Some(sig as i32),
                    }, cgroup_hierarchy, cgroup_monitor);
                }
            }
            Ok(WaitStatus::StillAlive) => break,
            Err(nix::errno::Errno::ECHILD) => break,
            Err(nix::errno::Errno::EINTR) => continue,
            _ => break,
        }
    }
}

/// Reap all zombie children and apply supervisor restart strategies.
fn reap_and_handle_exits(
    tree: &mut SupervisorTree,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};

    loop {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, code)) => {
                handle_child_exit(tree, pid.as_raw(), ExitInfo {
                    exit_code: Some(code),
                    signal: None,
                }, cgroup_hierarchy, cgroup_monitor);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                handle_child_exit(tree, pid.as_raw(), ExitInfo {
                    exit_code: None,
                    signal: Some(sig as i32),
                }, cgroup_hierarchy, cgroup_monitor);
            }
            Ok(WaitStatus::StillAlive) => break,
            Err(nix::errno::Errno::ECHILD) => break,
            Err(nix::errno::Errno::EINTR) => continue,
            _ => break,
        }
    }
}

/// Handle a single child process exit.
fn handle_child_exit(
    tree: &mut SupervisorTree,
    pid: i32,
    exit_info: ExitInfo,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    // Find which worker this PID belongs to
    let worker_id = match tree.unregister_pid(pid) {
        Some(id) => id,
        None => {
            tracing::debug!("reaped unknown pid {pid}");
            return;
        }
    };

    if let Some(code) = exit_info.exit_code {
        tracing::info!("service '{worker_id}' exited (code {code})");
    } else if let Some(sig) = exit_info.signal {
        tracing::warn!("service '{worker_id}' killed (signal {sig})");
    }

    // Update worker state
    if let Some(TreeNode::Worker(w)) = tree.get_mut(&worker_id) {
        w.pid = None;
        w.state = if exit_info.is_normal() {
            ServiceState::Stopped
        } else {
            ServiceState::Failed {
                exit_code: exit_info.exit_code,
                signal: exit_info.signal,
            }
        };
    }

    // Clean up cgroup for the exited service
    cgroup_monitor.unwatch(&worker_id);
    if let Some(hierarchy) = cgroup_hierarchy {
        let _ = hierarchy.remove_service_cgroup(&worker_id);
    }

    // Find the parent supervisor
    let supervisor_id = match tree.parent_of(&worker_id) {
        Some(id) => id.to_string(),
        None => return,
    };

    // Apply the supervisor's restart strategy
    let action = strategy::apply_strategy(tree, &supervisor_id, &worker_id, &exit_info);

    if action.supervisor_failed {
        tracing::error!("supervisor '{supervisor_id}' failed (intensity exceeded)");

        // Escalate to parent supervisor
        if let Some((parent_id, parent_action)) =
            strategy::escalate_failure(tree, &supervisor_id)
        {
            tracing::warn!("escalating failure to supervisor '{parent_id}'");
            execute_strategy_action(tree, &parent_action, cgroup_hierarchy, cgroup_monitor);
        } else {
            tracing::error!("root supervisor failed — system degraded");
        }
        return;
    }

    execute_strategy_action(tree, &action, cgroup_hierarchy, cgroup_monitor);
}

/// Execute the stop/start actions from a strategy decision.
fn execute_strategy_action(
    tree: &mut SupervisorTree,
    action: &strategy::StrategyAction,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    // Stop children that need stopping
    for child_id in &action.to_stop {
        if let Some(TreeNode::Worker(w)) = tree.get(child_id) {
            if w.state.is_running() {
                stop_worker(tree, child_id);
            }
        }
    }

    // Brief delay for stops to take effect
    if !action.to_stop.is_empty() {
        std::thread::sleep(Duration::from_millis(100));
        // Reap any stopped processes
        reap_and_handle_exits(tree, cgroup_hierarchy, cgroup_monitor);
    }

    // Start children that need starting
    for child_id in &action.to_start {
        // Get restart delay from the worker's config
        if let Some(w) = tree.get_worker(child_id) {
            let delay = parse_duration_secs(&w.def.restart.delay).unwrap_or(1);
            if delay > 0 {
                tracing::info!("waiting {delay}s before restarting '{child_id}'");
                std::thread::sleep(Duration::from_secs(delay));
            }
        }
        start_worker(tree, child_id, cgroup_hierarchy, cgroup_monitor);
    }
}

/// Reload service definitions: re-scan /etc/dynamod/services/,
/// add new services, remove deleted ones.
fn reload_services(
    tree: &mut SupervisorTree,
    cgroup_hierarchy: &Option<cgroup::hierarchy::CgroupHierarchy>,
    cgroup_monitor: &mut cgroup::monitor::CgroupMonitor,
) {
    let services_dir = std::path::Path::new(dynamod_common::paths::SERVICES_DIR);
    let new_defs = match service::load_services_dir(services_dir) {
        Ok(defs) => defs,
        Err(e) => {
            tracing::error!("reload: failed to load services: {e}");
            return;
        }
    };

    let existing: std::collections::HashSet<String> = tree
        .all_workers()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let loaded: std::collections::HashSet<String> = new_defs
        .iter()
        .map(|d| d.service.name.clone())
        .collect();

    // Start new services that weren't previously loaded
    let mut added = 0;
    for def in &new_defs {
        if !existing.contains(&def.service.name) {
            let parent = &def.service.supervisor;
            let parent_id = if tree.get(parent).is_some() {
                parent.as_str()
            } else {
                "root"
            };
            if let Err(e) = tree.add_worker(def.clone(), parent_id) {
                tracing::error!("reload: failed to add '{}': {e}", def.service.name);
                continue;
            }
            start_worker(tree, &def.service.name, cgroup_hierarchy, cgroup_monitor);
            added += 1;
        }
    }

    // Stop and remove services that no longer have config files
    let mut removed = 0;
    for name in &existing {
        if !loaded.contains(name) {
            stop_worker(tree, name);
            removed += 1;
        }
    }

    tracing::info!("reload complete: {added} added, {removed} removed");
}
