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

// --- Initramfs / switch_root constants ---

/// Mount point for the real root filesystem during switch_root.
pub const newroot_path: [*:0]const u8 = "/newroot";

/// Kernel command line path (available after /proc is mounted).
pub const proc_cmdline_path = "/proc/cmdline";

/// Filesystem magic numbers from statfs(2) for initramfs detection.
pub const RAMFS_MAGIC: i64 = 0x858458f6;
pub const TMPFS_MAGIC: i64 = 0x01021994;

/// Mount flag: move an existing mount to a new location.
pub const MS_MOVE: u32 = 0x2000;
pub const MS_RDONLY: u32 = 0x01;
pub const MS_REMOUNT: u32 = 0x20;

/// How often to poll for root device when rootwait is set (ms).
pub const rootwait_poll_ms: u32 = 250;

/// Maximum time to wait for root device (seconds).
pub const rootwait_max_s: u32 = 30;

/// Init binary path (used for re-exec after switch_root).
pub const init_path: [*:0]const u8 = "/sbin/dynamod-init";

/// Path to mdev binary (busybox applet, for device node creation).
pub const mdev_path: [*:0]const u8 = "/sbin/mdev";

// --- ISO / live boot staging (under tmpfs /run) ---

pub const live_staging_base = "/run/dynamod/live";
pub const live_iso_mp = "/run/dynamod/live/iso";
pub const live_squash_mp = "/run/dynamod/live/squash";
pub const live_upper_mp = "/run/dynamod/live/upper";
pub const live_work_mp = "/run/dynamod/live/work";
