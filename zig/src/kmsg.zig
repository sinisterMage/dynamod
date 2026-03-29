/// Kernel log (/dev/kmsg) writer for dynamod-init.
/// All logging goes through here since we cannot rely on userspace logging
/// being available during early boot or late shutdown.
const std = @import("std");
const linux = std.os.linux;

const Self = @This();

fd: std.posix.fd_t,

/// Open /dev/kmsg for writing. Returns null if it cannot be opened
/// (e.g. /dev is not yet mounted).
pub fn init() ?Self {
    const fd = std.posix.open("/dev/kmsg", .{ .ACCMODE = .WRONLY, .NOCTTY = true }, 0) catch return null;
    return Self{ .fd = fd };
}

/// Log a message to the kernel ring buffer.
/// Format: "<priority>dynamod-init: message\n"
/// Priority levels follow syslog: 0=emerg, 3=err, 4=warn, 6=info, 7=debug
pub fn log(self: Self, comptime priority: u8, comptime fmt: []const u8, args: anytype) void {
    var buf: [1024]u8 = undefined;
    const prefix = comptime std.fmt.comptimePrint("<{d}>dynamod-init: ", .{priority});
    const msg = std.fmt.bufPrint(&buf, prefix ++ fmt ++ "\n", args) catch return;
    _ = std.posix.write(self.fd, msg) catch {};
}

pub fn info(self: Self, comptime fmt: []const u8, args: anytype) void {
    self.log(6, fmt, args);
}

pub fn err(self: Self, comptime fmt: []const u8, args: anytype) void {
    self.log(3, fmt, args);
}

pub fn warn(self: Self, comptime fmt: []const u8, args: anytype) void {
    self.log(4, fmt, args);
}

pub fn emerg(self: Self, comptime fmt: []const u8, args: anytype) void {
    self.log(0, fmt, args);
}
