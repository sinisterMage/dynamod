/// Well-known paths and constants for dynamod-init.
pub const run_dir = "/run/dynamod";
pub const control_sock = "/run/dynamod/control.sock";
pub const svmgr_path = "/usr/lib/dynamod/dynamod-svmgr";
pub const hostname_path = "/etc/hostname";
pub const random_seed_path = "/var/lib/dynamod/random-seed";
pub const machine_id_path = "/etc/machine-id";

/// Environment variable name for passing the init socket fd to svmgr.
pub const init_fd_env = "DYNAMOD_INIT_FD";

/// IPC protocol magic bytes: "DM"
pub const ipc_magic: [2]u8 = .{ 0x44, 0x4D };

/// Maximum IPC message size: 64 KiB
pub const max_message_size: u32 = 64 * 1024;

/// Heartbeat interval in seconds.
pub const heartbeat_interval_s: u32 = 5;

/// Shutdown timeout: seconds to wait after SIGTERM before SIGKILL.
pub const shutdown_sigterm_timeout_s: u32 = 5;

/// Seconds to wait after SIGKILL before proceeding.
pub const shutdown_sigkill_timeout_s: u32 = 2;

/// Svmgr restart backoff: initial delay in milliseconds.
pub const svmgr_restart_initial_ms: u32 = 500;

/// Svmgr restart backoff: maximum delay in milliseconds.
pub const svmgr_restart_max_ms: u32 = 30_000;
