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
    // Blocking writes: with O_NONBLOCK, a busy kmsg ring buffer returns EAGAIN and we would drop
    // lines silently (write ... catch {}), which looked like "zig never reached" the next step.
    const fd = std.posix.open("/dev/kmsg", .{ .ACCMODE = .WRONLY, .NOCTTY = true }, 0) catch return null;
    return Self{ .fd = fd };
}

fn writeAllKmsg(fd: std.posix.fd_t, buf: []const u8) void {
    var off: usize = 0;
    while (off < buf.len) {
        const n = std.posix.write(fd, buf[off..]) catch return;
        if (n == 0) return;
        off += n;
    }
}

/// Log a message to the kernel ring buffer.
/// Format: "<priority>dynamod-init: message\n"
/// Priority levels follow syslog: 0=emerg, 3=err, 4=warn, 6=info, 7=debug
pub fn log(self: Self, comptime priority: u8, comptime fmt: []const u8, args: anytype) void {
    // Large enough for long paths + progress lines; bufPrint failure would drop the line silently.
    var buf: [2048]u8 = undefined;
    const prefix = comptime std.fmt.comptimePrint("<{d}>dynamod-init: ", .{priority});
    const msg = std.fmt.bufPrint(&buf, prefix ++ fmt ++ "\n", args) catch return;
    writeAllKmsg(self.fd, msg);
}

/// Short fixed line (no fmt); avoids bufPrint failure and keeps breadcrumbs minimal.
pub fn infoLiteral(self: Self, body: []const u8) void {
    const pfx = "<6>dynamod-init: ";
    var buf: [320]u8 = undefined;
    if (pfx.len + body.len + 1 > buf.len) return;
    @memcpy(buf[0..pfx.len], pfx);
    @memcpy(buf[pfx.len..][0..body.len], body);
    buf[pfx.len + body.len] = '\n';
    writeAllKmsg(self.fd, buf[0 .. pfx.len + body.len + 1]);
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
