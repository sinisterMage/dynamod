/// Kernel command line parser for initramfs boot parameters.
///
/// Reads /proc/cmdline and extracts key=value pairs needed for
/// root device detection and mounting. Values are zero-copy slices
/// into the internal buffer.
const std = @import("std");
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

pub const Cmdline = struct {
    buf: [4096]u8 = undefined,
    len: usize = 0,

    /// Read and parse /proc/cmdline. Must be called after /proc is mounted.
    pub fn read(klog_arg: ?kmsg) Cmdline {
        var self = Cmdline{};
        const file = std.fs.openFileAbsolute(constants.proc_cmdline_path, .{}) catch {
            if (klog_arg) |k| k.warn("cannot open {s}", .{constants.proc_cmdline_path});
            return self;
        };
        defer file.close();

        self.len = file.read(&self.buf) catch {
            if (klog_arg) |k| k.warn("cannot read {s}", .{constants.proc_cmdline_path});
            return self;
        };

        // Trim trailing newline
        while (self.len > 0 and (self.buf[self.len - 1] == '\n' or self.buf[self.len - 1] == '\r')) {
            self.len -= 1;
        }

        return self;
    }

    /// Get the value of a key=value parameter, or null if not found.
    pub fn getParam(self: *const Cmdline, key: []const u8) ?[]const u8 {
        const data = self.buf[0..self.len];
        var iter = std.mem.tokenizeScalar(u8, data, ' ');
        while (iter.next()) |token| {
            // Check for key=value
            if (std.mem.indexOfScalar(u8, token, '=')) |eq_pos| {
                if (std.mem.eql(u8, token[0..eq_pos], key)) {
                    return token[eq_pos + 1 ..];
                }
            }
        }
        return null;
    }

    /// Check if a bare flag (no =value) is present on the command line.
    pub fn hasFlag(self: *const Cmdline, flag: []const u8) bool {
        const data = self.buf[0..self.len];
        var iter = std.mem.tokenizeScalar(u8, data, ' ');
        while (iter.next()) |token| {
            if (std.mem.eql(u8, token, flag)) return true;
        }
        return false;
    }

    /// Get the root= parameter value.
    pub fn getRoot(self: *const Cmdline) ?[]const u8 {
        return self.getParam("root");
    }

    /// Get the rootfstype= parameter value (e.g., "ext4").
    pub fn getRootFsType(self: *const Cmdline) ?[]const u8 {
        return self.getParam("rootfstype");
    }

    /// Get the rootflags= parameter value (e.g., "ro,noatime").
    pub fn getRootFlags(self: *const Cmdline) ?[]const u8 {
        return self.getParam("rootflags");
    }

    /// Check if rootwait is specified (wait indefinitely for root device).
    pub fn hasRootwait(self: *const Cmdline) bool {
        return self.hasFlag("rootwait") or self.getParam("rootwait") != null;
    }

    /// Get rootdelay= value in seconds (0 if not specified).
    pub fn getRootDelay(self: *const Cmdline) u32 {
        const val = self.getParam("rootdelay") orelse return 0;
        return std.fmt.parseInt(u32, val, 10) catch 0;
    }

    /// Get the raw command line as a string slice.
    pub fn raw(self: *const Cmdline) []const u8 {
        return self.buf[0..self.len];
    }
};

// --- Tests ---

test "parse simple cmdline" {
    var cl = Cmdline{};
    const input = "console=ttyS0 root=/dev/vda1 rootfstype=ext4 rootwait";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expectEqualStrings("/dev/vda1", cl.getRoot().?);
    try std.testing.expectEqualStrings("ext4", cl.getRootFsType().?);
    try std.testing.expect(cl.hasRootwait());
    try std.testing.expectEqualStrings("ttyS0", cl.getParam("console").?);
    try std.testing.expect(cl.getRootFlags() == null);
}

test "parse UUID root" {
    var cl = Cmdline{};
    const input = "root=UUID=abcd-1234 rootfstype=btrfs rootflags=ro,compress";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expectEqualStrings("UUID=abcd-1234", cl.getRoot().?);
    try std.testing.expectEqualStrings("btrfs", cl.getRootFsType().?);
    try std.testing.expectEqualStrings("ro,compress", cl.getRootFlags().?);
    try std.testing.expect(!cl.hasRootwait());
}

test "parse empty cmdline" {
    var cl = Cmdline{};
    cl.len = 0;

    try std.testing.expect(cl.getRoot() == null);
    try std.testing.expect(!cl.hasRootwait());
    try std.testing.expect(cl.getRootDelay() == 0);
}

test "parse rootdelay" {
    var cl = Cmdline{};
    const input = "rootdelay=5 root=/dev/sda1";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expect(cl.getRootDelay() == 5);
}
