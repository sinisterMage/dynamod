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
    restart_timer = 4,
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

    while (true) {
        const nfds = linux.epoll_pwait(self.epoll_fd, &events, events.len, -1, null);
        const wait_err = linux.E.init(nfds);

        if (wait_err == .INTR) continue;
        if (wait_err != .SUCCESS) {
            if (self.klog) |k| k.err("epoll_pwait failed: errno {d}", .{@intFromEnum(wait_err)});
            continue;
        }

        const n: usize = @intCast(nfds);
        for (events[0..n]) |ev| {
            const source: EventSource = @enumFromInt(ev.data.u64);
            switch (source) {
                .signal_fd => self.handleSignals(),
                .svmgr_pidfd => self.handleSvmgrExit(),
                .init_sock => self.handleSvmgrMessage(),
                .restart_timer => self.handleRestartTimer(),
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

    // Read data into the IPC buffer
    const space = self.ipc_buf.buf[self.ipc_buf.len..];
    if (space.len == 0) {
        self.ipc_buf.len = 0;
        return;
    }

    const n = std.posix.read(sock, space) catch |e| {
        if (e == error.WouldBlock) return;
        return;
    };

    if (n == 0) return; // EOF
    self.ipc_buf.len += n;

    // Process complete messages
    const constants_mod = @import("constants.zig");
    const HEADER_SIZE = 6;

    while (self.ipc_buf.len >= HEADER_SIZE) {
        if (self.ipc_buf.buf[0] != constants_mod.ipc_magic[0] or self.ipc_buf.buf[1] != constants_mod.ipc_magic[1]) {
            // Bad magic — reset
            self.ipc_buf.len = 0;
            return;
        }

        const payload_len = std.mem.readInt(u32, self.ipc_buf.buf[2..6], .little);
        if (payload_len > constants_mod.max_message_size) {
            self.ipc_buf.len = 0;
            return;
        }

        const total = HEADER_SIZE + payload_len;
        if (self.ipc_buf.len < total) break;

        // Parse and handle the message
        const payload = self.ipc_buf.buf[HEADER_SIZE..total];
        self.dispatchMessage(payload);

        // Shift remaining data
        if (self.ipc_buf.len > total) {
            std.mem.copyForwards(u8, &self.ipc_buf.buf, self.ipc_buf.buf[total..self.ipc_buf.len]);
        }
        self.ipc_buf.len -= total;
    }
}

/// Parse and dispatch a single IPC message payload.
fn dispatchMessage(self: *Self, payload: []const u8) void {
    const msgpack = @import("msgpack.zig");

    // Look for "body" field in the msgpack map
    const body_raw = msgpack.lookupMapString(payload, "body") orelse {
        if (self.klog) |k| k.warn("IPC: no body field", .{});
        return;
    };

    const body_result = msgpack.decode(body_raw) catch {
        if (self.klog) |k| k.warn("IPC: failed to decode body", .{});
        return;
    };

    switch (body_result.value) {
        .string => |s| {
            if (std.mem.eql(u8, s, "Heartbeat")) {
                if (self.svmgr.getInitSockFd()) |fd| {
                    ipc.sendHeartbeatAck(fd, 0);
                }
            }
        },
        else => {
            // Check for RequestShutdown
            if (msgpack.lookupMapString(body_raw, "RequestShutdown")) |shutdown_raw| {
                const kind_raw = msgpack.lookupMapString(shutdown_raw, "kind") orelse return;
                const kind_result = msgpack.decode(kind_raw) catch return;
                if (kind_result.value == .string) {
                    const kind_str = kind_result.value.string;
                    if (std.mem.eql(u8, kind_str, "Poweroff")) {
                        if (self.klog) |k| k.info("svmgr requested poweroff", .{});
                        self.shutdown_requested = .poweroff;
                    } else if (std.mem.eql(u8, kind_str, "Reboot")) {
                        if (self.klog) |k| k.info("svmgr requested reboot", .{});
                        self.shutdown_requested = .reboot;
                    } else if (std.mem.eql(u8, kind_str, "Halt")) {
                        if (self.klog) |k| k.info("svmgr requested halt", .{});
                        self.shutdown_requested = .halt;
                    }
                }
            }
            // Check for LogToKmsg
            else if (msgpack.lookupMapString(body_raw, "LogToKmsg")) |log_raw| {
                const msg_raw = msgpack.lookupMapString(log_raw, "message") orelse return;
                const msg_result = msgpack.decode(msg_raw) catch return;
                if (msg_result.value == .string) {
                    if (self.klog) |k| k.info("svmgr: {s}", .{msg_result.value.string});
                }
            }
        },
    }
}

fn handleRestartTimer(self: *Self) void {
    _ = self;
    // Will be used for timerfd-based restart scheduling in a future phase
}
