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
const emergency = @import("emergency.zig");
const constants = @import("constants.zig");

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
/// Set once after the emergency shell exits and svmgr has been respawned.
/// A subsequent svmgr exit inside the post-emergency grace window triggers
/// a reboot — the operator's fix did not hold.
post_emergency_armed: bool,
/// Set when the operator triggers an emergency shell via SIGUSR2 and we
/// have asked the currently-running svmgr to terminate. handleSvmgrExit
/// observes this flag and enters emergency mode instead of restarting.
pending_emergency: bool,

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
        .post_emergency_armed = false,
        .pending_emergency = false,
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
                if (self.klog) |k| k.warn("received SIGUSR2 — operator requested emergency shell", .{});
                if (self.svmgr.pid) |pid| {
                    // Ask svmgr to terminate; the emergency shell will be
                    // started from handleSvmgrExit when SIGCHLD fires.
                    self.svmgr.markStopping();
                    self.pending_emergency = true;
                    _ = linux.kill(pid, linux.SIG.TERM);
                } else {
                    // svmgr is already dead — run the shell immediately.
                    self.enterEmergencyMode();
                }
            },
            .unknown => {},
        }
    }
}

fn handleSvmgrExit(self: *Self) void {
    if (self.klog) |k| k.warn("dynamod-svmgr process exited", .{});

    // Unregister old fds, handle exit
    self.unregisterSvmgr();
    const delay_ms = self.svmgr.onExit(self.klog);

    // Reset IPC buffer for new connection
    self.ipc_buf.len = 0;

    // Operator triggered an emergency via SIGUSR2 and we asked svmgr to
    // terminate — enter the emergency shell now instead of restarting.
    if (self.pending_emergency) {
        self.pending_emergency = false;
        self.enterEmergencyMode();
        return;
    }

    // svmgr crashed after a recent emergency recovery. If it failed
    // quickly, the operator's fix didn't hold — reboot. If it survived
    // the grace window, clear the flag and resume normal restarts.
    if (self.post_emergency_armed) {
        const uptime_ms = std.time.milliTimestamp() - self.svmgr.spawn_time_ms;
        if (uptime_ms < constants.svmgr_post_emergency_grace_ms) {
            if (self.klog) |k| k.emerg("svmgr died {d}ms after emergency recovery — rebooting", .{uptime_ms});
            self.shutdown_requested = .reboot;
            return;
        }
        self.post_emergency_armed = false;
        if (self.klog) |k| k.info("svmgr survived post-emergency grace window; resuming normal restart", .{});
    }

    // Crash loop: fall back to the emergency shell so the operator can
    // recover the system instead of letting init spin forever.
    if (self.svmgr.isInCrashLoop()) {
        if (self.klog) |k| k.emerg("svmgr crash loop detected ({d} rapid failures) — entering emergency shell", .{self.svmgr.rapid_failure_count});
        self.enterEmergencyMode();
        return;
    }

    // Normal exponential-backoff restart.
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

/// Run an emergency shell on /dev/console, then respawn svmgr once.
/// If the respawn fails (or svmgr crashes again inside the grace window
/// per handleSvmgrExit), the system reboots.
///
/// Callable from three trigger paths: crash-loop fallback, SIGUSR2, and
/// `dynamod.emergency` on the kernel command line (via enterEmergencyBoot).
fn enterEmergencyMode(self: *Self) void {
    // The emergency shell blocks init. svmgr must already be stopped at
    // this point — pending_emergency / crash-loop paths handle that, and
    // the cmdline path never spawned svmgr in the first place.
    _ = emergency.runShell(self.klog);

    if (self.klog) |k| k.warn("emergency shell exited; respawning svmgr (post-emergency mode)", .{});

    // The operator presumably fixed whatever was wrong — give svmgr a
    // clean slate, but arm the post-emergency check so a quick re-crash
    // forces a reboot rather than another emergency loop.
    self.svmgr.resetFailureCounter();
    self.post_emergency_armed = true;

    self.svmgr.spawn(self.klog) catch |e| {
        if (self.klog) |k| k.emerg("post-emergency svmgr spawn failed: {s} — rebooting", .{@errorName(e)});
        self.shutdown_requested = .reboot;
        return;
    };
    self.registerSvmgr() catch |e| {
        if (self.klog) |k| k.warn("failed to register svmgr fds post-emergency: {s}", .{@errorName(e)});
    };
}

/// Entry point for `dynamod.emergency=1` on the kernel command line.
/// Called by main.zig before the event loop runs, when svmgr has not yet
/// been spawned.
pub fn enterEmergencyBoot(self: *Self) void {
    if (self.klog) |k| k.warn("dynamod.emergency set on cmdline — booting into emergency shell", .{});
    self.enterEmergencyMode();
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

