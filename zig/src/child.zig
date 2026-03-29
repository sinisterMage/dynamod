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

pub fn create(svmgr_path: [*:0]const u8) Self {
    return Self{
        .pid = null,
        .pidfd = null,
        .init_sock_fd = null,
        .backoff_ms = constants.svmgr_restart_initial_ms,
        .svmgr_path = svmgr_path,
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

        // Build environment string for execve
        var env_buf: [128]u8 = undefined;
        const env_str = std.fmt.bufPrintZ(&env_buf, "{s}={s}", .{ constants.init_fd_env, fd_str }) catch unreachable;

        const argv = [_:null]?[*:0]const u8{
            self.svmgr_path,
            null,
        };
        const env = [_:null]?[*:0]const u8{
            env_str,
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
}

/// Called when svmgr exits. Increases the restart backoff.
/// Returns the backoff delay in milliseconds before the next restart attempt.
pub fn onExit(self: *Self, klog: ?kmsg) u32 {
    const delay = self.backoff_ms;

    if (klog) |k| k.warn("dynamod-svmgr exited, will restart in {d}ms", .{delay});

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

/// Get the pidfd for epoll registration.
pub fn getPidfd(self: Self) ?std.posix.fd_t {
    return self.pidfd;
}

/// Get the init-side socket fd.
pub fn getInitSockFd(self: Self) ?std.posix.fd_t {
    return self.init_sock_fd;
}
