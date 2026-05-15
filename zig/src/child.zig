/// Spawn and monitor the service manager (dynamod-svmgr) child process.
/// If svmgr crashes, we restart it with exponential backoff.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

const Self = @This();

/// PID of the running svmgr process, or null if not running.
pid: ?linux.pid_t,
/// pidfd for epoll monitoring.
pidfd: ?std.posix.fd_t,
/// Socket fd for communicating with svmgr (init's end of the socketpair).
init_sock_fd: ?std.posix.fd_t,
/// Current restart backoff in milliseconds.
backoff_ms: u32,
/// Path to svmgr binary.
svmgr_path: [*:0]const u8,
/// Wall-clock time (ms since epoch) when svmgr was most recently spawned.
/// Used by onExit to decide whether an exit counts as a "rapid" failure.
spawn_time_ms: i64,
/// Count of consecutive rapid failures. Reset whenever svmgr survives
/// `svmgr_rapid_failure_uptime_ms` before exiting, or by the emergency
/// recovery path after an operator intervention.
rapid_failure_count: u32,
/// Set by event_loop before sending SIGTERM to svmgr for a deliberate stop
/// (e.g. SIGUSR2 emergency-shell trigger). Tells onExit not to charge this
/// exit against the rapid-failure counter.
intentional_stop: bool,

pub fn create(svmgr_path: [*:0]const u8) Self {
    return Self{
        .pid = null,
        .pidfd = null,
        .init_sock_fd = null,
        .backoff_ms = constants.svmgr_restart_initial_ms,
        .svmgr_path = svmgr_path,
        .spawn_time_ms = 0,
        .rapid_failure_count = 0,
        .intentional_stop = false,
    };
}

/// Spawn the service manager process.
/// Creates a Unix socketpair for IPC, then fork/execs svmgr.
pub fn spawn(self: *Self, klog: ?kmsg) !void {
    // Create socketpair for init <-> svmgr communication
    var fds: [2]i32 = undefined;
    const sp_rc = linux.socketpair(linux.AF.UNIX, @bitCast(@as(u32, linux.SOCK.STREAM | linux.SOCK.CLOEXEC)), 0, &fds);
    const sp_err = linux.E.init(sp_rc);
    if (sp_err != .SUCCESS) return error.SocketpairFailed;
    const init_fd = fds[0];
    const svmgr_fd = fds[1];

    const fork_rc = linux.fork();
    const fork_err = linux.E.init(fork_rc);
    if (fork_err != .SUCCESS) {
        std.posix.close(init_fd);
        std.posix.close(svmgr_fd);
        return error.ForkFailed;
    }

    const pid: linux.pid_t = @intCast(fork_rc);

    if (pid == 0) {
        // === Child process ===
        std.posix.close(init_fd);

        // Clear CLOEXEC on the svmgr fd so it survives exec
        const flags = linux.fcntl(svmgr_fd, linux.F.GETFD, @as(u32, 0));
        const flags_err = linux.E.init(flags);
        if (flags_err == .SUCCESS) {
            _ = linux.fcntl(svmgr_fd, linux.F.SETFD, @as(u32, @intCast(flags)) & ~@as(u32, linux.FD_CLOEXEC));
        }

        // Set DYNAMOD_INIT_FD environment variable
        var fd_buf: [16]u8 = undefined;
        const fd_str = std.fmt.bufPrintZ(&fd_buf, "{d}", .{svmgr_fd}) catch "3";

        // Build environment for execve
        var env_buf: [128]u8 = undefined;
        const env_str = std.fmt.bufPrintZ(&env_buf, "{s}={s}", .{ constants.init_fd_env, fd_str }) catch unreachable;

        const argv = [_:null]?[*:0]const u8{
            self.svmgr_path,
            null,
        };
        const env = [_:null]?[*:0]const u8{
            env_str,
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/",
            "TERM=linux",
            null,
        };

        const rc = linux.execve(self.svmgr_path, &argv, &env);
        // If we get here, exec failed
        const e = linux.E.init(rc);
        _ = e;
        // Write error to stderr and exit
        _ = std.posix.write(2, "dynamod-init: failed to exec svmgr\n") catch {};
        linux.exit(127);
    }

    // === Parent process ===
    std.posix.close(svmgr_fd);
    self.pid = pid;
    self.init_sock_fd = init_fd;

    // Open pidfd for the child
    const pidfd_rc = linux.pidfd_open(pid, 0);
    const pidfd_err = linux.E.init(pidfd_rc);
    if (pidfd_err == .SUCCESS) {
        self.pidfd = @intCast(pidfd_rc);
    }

    if (klog) |k| k.info("spawned dynamod-svmgr (pid {d})", .{pid});

    // Reset backoff on successful spawn
    self.backoff_ms = constants.svmgr_restart_initial_ms;
    self.spawn_time_ms = std.time.milliTimestamp();
}

/// Called when svmgr exits. Increases the restart backoff and updates the
/// rapid-failure counter. Returns the backoff delay in milliseconds before
/// the next restart attempt.
pub fn onExit(self: *Self, klog: ?kmsg) u32 {
    const delay = self.backoff_ms;

    // Update the rapid-failure counter unless this was a deliberate stop.
    if (self.intentional_stop) {
        self.intentional_stop = false;
    } else if (self.spawn_time_ms != 0) {
        const uptime_ms = std.time.milliTimestamp() - self.spawn_time_ms;
        if (uptime_ms < constants.svmgr_rapid_failure_uptime_ms) {
            self.rapid_failure_count += 1;
        } else {
            self.rapid_failure_count = 0;
        }
    }

    if (klog) |k| k.warn("dynamod-svmgr exited, will restart in {d}ms (rapid_failures={d})", .{ delay, self.rapid_failure_count });

    // Exponential backoff
    self.backoff_ms = @min(self.backoff_ms * 2, constants.svmgr_restart_max_ms);

    // Clean up
    if (self.pidfd) |fd| std.posix.close(fd);
    if (self.init_sock_fd) |fd| std.posix.close(fd);
    self.pid = null;
    self.pidfd = null;
    self.init_sock_fd = null;

    return delay;
}

/// Predicate: should we drop to an emergency shell instead of restarting?
pub fn isInCrashLoop(self: Self) bool {
    return self.rapid_failure_count >= constants.svmgr_rapid_failure_threshold;
}

/// Reset failure tracking after an operator has intervened.
pub fn resetFailureCounter(self: *Self) void {
    self.rapid_failure_count = 0;
    self.backoff_ms = constants.svmgr_restart_initial_ms;
}

/// Mark the next svmgr exit as intentional (e.g. SIGUSR2 emergency trigger)
/// so it does not count against the rapid-failure counter.
pub fn markStopping(self: *Self) void {
    self.intentional_stop = true;
}

/// Get the pidfd for epoll registration.
pub fn getPidfd(self: Self) ?std.posix.fd_t {
    return self.pidfd;
}

/// Get the init-side socket fd.
pub fn getInitSockFd(self: Self) ?std.posix.fd_t {
    return self.init_sock_fd;
}
