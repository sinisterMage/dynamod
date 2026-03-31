/// epoll-based main event loop for dynamod-init.
/// Monitors: signalfd (signals), pidfd (svmgr exit), init_sock (IPC from svmgr).
const std = @import("std");
const linux = std.os.linux;
const kmsg = @import("kmsg.zig");
const signal = @import("signal.zig");
const reaper = @import("reaper.zig");
const child = @import("child.zig");
const shutdown = @import("shutdown.zig");
const ipc = @import("ipc.zig");

const Self = @This();

/// Event source identifiers stored in epoll_event.data.
const EventSource = enum(u64) {
    signal_fd = 1,
    svmgr_pidfd = 2,
    init_sock = 3,
};

epoll_fd: std.posix.fd_t,
sig: signal,
svmgr: *child,
klog: ?kmsg,
shutdown_requested: ?shutdown.ShutdownKind,
ipc_buf: ipc.ReadBuffer,

pub fn init(sig_handler: signal, svmgr_child: *child, klog_writer: ?kmsg) !Self {
    const epfd = linux.epoll_create1(linux.EPOLL.CLOEXEC);
    const e = linux.E.init(epfd);
    if (e != .SUCCESS) return error.EpollCreateFailed;

    var self = Self{
        .epoll_fd = @intCast(epfd),
        .sig = sig_handler,
        .svmgr = svmgr_child,
        .klog = klog_writer,
        .shutdown_requested = null,
        .ipc_buf = .{},
    };

    // Register signalfd
    try self.addFd(sig_handler.fd, .signal_fd);

    return self;
}

fn addFd(self: *Self, fd: std.posix.fd_t, source: EventSource) !void {
    var ev = linux.epoll_event{
        .events = linux.EPOLL.IN,
        .data = .{ .u64 = @intFromEnum(source) },
    };
    const rc = linux.epoll_ctl(self.epoll_fd, linux.EPOLL.CTL_ADD, fd, &ev);
    const e = linux.E.init(rc);
    if (e != .SUCCESS) return error.EpollCtlFailed;
}

fn removeFd(self: *Self, fd: std.posix.fd_t) void {
    _ = linux.epoll_ctl(self.epoll_fd, linux.EPOLL.CTL_DEL, fd, null);
}

/// Register the svmgr's pidfd and socket with epoll.
pub fn registerSvmgr(self: *Self) !void {
    if (self.svmgr.getPidfd()) |pidfd| {
        try self.addFd(pidfd, .svmgr_pidfd);
    }
    if (self.svmgr.getInitSockFd()) |sock| {
        try self.addFd(sock, .init_sock);
    }
}

/// Unregister svmgr fds from epoll (called before svmgr restart).
pub fn unregisterSvmgr(self: *Self) void {
    if (self.svmgr.getPidfd()) |pidfd| {
        self.removeFd(pidfd);
    }
    if (self.svmgr.getInitSockFd()) |sock| {
        self.removeFd(sock);
    }
}

/// Run the main event loop. Does not return under normal operation.
pub fn run(self: *Self) noreturn {
    var events: [8]linux.epoll_event = undefined;
    var consecutive_errors: u32 = 0;

    while (true) {
        const nfds = linux.epoll_pwait(self.epoll_fd, &events, events.len, -1, null);
        const wait_err = linux.E.init(nfds);

        if (wait_err == .INTR) continue;
        if (wait_err != .SUCCESS) {
            consecutive_errors += 1;
            if (self.klog) |k| k.err("epoll_pwait failed: errno {d} (consecutive: {d})", .{ @intFromEnum(wait_err), consecutive_errors });

            if (consecutive_errors >= 100) {
                if (self.klog) |k| k.emerg("epoll_pwait failing persistently, initiating shutdown", .{});
                shutdown.execute(.poweroff, self.klog);
            }

            // Exponential backoff: 10ms, 20ms, 40ms, ... capped at 1s
            const backoff_ms: u64 = @min(1000, @as(u64, 10) << @intCast(@min(consecutive_errors, 6)));
            const ts = linux.timespec{
                .sec = @intCast(backoff_ms / 1000),
                .nsec = @intCast((backoff_ms % 1000) * 1_000_000),
            };
            _ = linux.nanosleep(&ts, null);
            continue;
        }

        consecutive_errors = 0;

        const n: usize = @intCast(nfds);
        for (events[0..n]) |ev| {
            const source: EventSource = @enumFromInt(ev.data.u64);
            switch (source) {
                .signal_fd => self.handleSignals(),
                .svmgr_pidfd => self.handleSvmgrExit(),
                .init_sock => self.handleSvmgrMessage(),
            }
        }

        // If shutdown was requested, execute it
        if (self.shutdown_requested) |kind| {
            shutdown.execute(kind, self.klog);
        }
    }
}

fn handleSignals(self: *Self) void {
    while (self.sig.read()) |ev| {
        switch (ev) {
            .child_exited => {
                _ = reaper.reapAll(self.klog, null);
                // If pidfd is unavailable, detect svmgr exit via SIGCHLD fallback:
                // check if our svmgr PID was reaped (kill(pid, 0) fails with ESRCH)
                if (self.svmgr.getPidfd() == null) {
                    if (self.svmgr.pid) |pid| {
                        const rc = linux.kill(pid, 0);
                        if (linux.E.init(rc) == .SRCH) {
                            self.handleSvmgrExit();
                        }
                    }
                }
            },
            .shutdown_term => {
                if (self.klog) |k| k.info("received SIGTERM", .{});
                // Notify svmgr before shutting down
                if (self.svmgr.getInitSockFd()) |sock| {
                    ipc.sendShutdownSignal(sock, "SIGTERM");
                }
                self.shutdown_requested = .poweroff;
            },
            .shutdown_int => {
                if (self.klog) |k| k.info("received SIGINT", .{});
                if (self.svmgr.getInitSockFd()) |sock| {
                    ipc.sendShutdownSignal(sock, "SIGINT");
                }
                self.shutdown_requested = .poweroff;
            },
            .user1 => {
                if (self.klog) |k| k.info("received SIGUSR1 (reboot)", .{});
                if (self.svmgr.getInitSockFd()) |sock| {
                    ipc.sendShutdownSignal(sock, "SIGUSR1");
                }
                self.shutdown_requested = .reboot;
            },
            .user2 => {
                if (self.klog) |k| k.info("received SIGUSR2", .{});
            },
            .unknown => {},
        }
    }
}

fn handleSvmgrExit(self: *Self) void {
    if (self.klog) |k| k.warn("dynamod-svmgr process exited", .{});

    // Unregister old fds, handle exit, and restart
    self.unregisterSvmgr();
    const delay_ms = self.svmgr.onExit(self.klog);

    // Reset IPC buffer for new connection
    self.ipc_buf.len = 0;

    // Sleep for backoff delay then restart
    const ts = linux.timespec{
        .sec = @intCast(delay_ms / 1000),
        .nsec = @intCast((@as(u64, delay_ms) % 1000) * 1_000_000),
    };
    _ = linux.nanosleep(&ts, null);

    self.svmgr.spawn(self.klog) catch |e| {
        if (self.klog) |k| k.err("failed to restart svmgr: {s}", .{@errorName(e)});
        return;
    };

    self.registerSvmgr() catch |e| {
        if (self.klog) |k| k.err("failed to register svmgr fds: {s}", .{@errorName(e)});
    };
}

fn handleSvmgrMessage(self: *Self) void {
    const sock = self.svmgr.getInitSockFd() orelse return;

    self.ipc_buf.readFrom(sock) catch |e| {
        if (e == error.EndOfStream) {
            if (self.klog) |k| k.warn("IPC: svmgr closed connection", .{});
        }
        return;
    };

    while (self.ipc_buf.nextMessage()) |msg| {
        switch (msg) {
            .heartbeat => {
                if (self.svmgr.getInitSockFd()) |fd| {
                    ipc.sendHeartbeatAck(fd, 0);
                }
            },
            .request_shutdown => |kind| {
                if (self.klog) |k| k.info("svmgr requested {s}", .{@tagName(kind)});
                self.shutdown_requested = kind;
            },
            .log_to_kmsg => |log| {
                if (self.klog) |k| {
                    switch (log.level) {
                        0...3 => k.err("svmgr: {s}", .{log.message}),
                        4 => k.warn("svmgr: {s}", .{log.message}),
                        else => k.info("svmgr: {s}", .{log.message}),
                    }
                }
            },
            .unknown => {},
        }
    }
}

