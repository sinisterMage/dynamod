/// Kernel command line parser for initramfs boot parameters.
///
/// Reads /proc/cmdline and extracts key=value pairs needed for
/// root device detection and mounting. Values are zero-copy slices
/// into the internal buffer.
const std = @import("std");
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

pub const LiveSquashPread = struct {
    start_byte: u64,
    size: u64,
};

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

    /// True when dynamod ISO/live pipeline is enabled (dynamod.live flag or =1/true/yes).
    pub fn isLive(self: *const Cmdline) bool {
        if (self.hasFlag("dynamod.live")) return true;
        const v = self.getParam("dynamod.live") orelse return false;
        return std.mem.eql(u8, v, "1") or
            std.mem.eql(u8, v, "true") or
            std.mem.eql(u8, v, "yes");
    }

    /// Block device holding the ISO (LABEL=, UUID=, /dev/...), same grammar as root=.
    pub fn getLiveMedia(self: *const Cmdline) ?[]const u8 {
        return self.getParam("dynamod.media");
    }

    /// Path to squashfs image inside the mounted ISO (default: /live/root.squashfs).
    pub fn getLiveSquashfsPath(self: *const Cmdline) []const u8 {
        return self.getParam("dynamod.squashfs") orelse "/live/root.squashfs";
    }

    /// ISO 9660 logical block (2048-byte sectors) and exact file size for `/live/root.squashfs`,
    /// from `dynamod.squash_pread=LBA:BYTES` (build-iso patches this into the image).
    /// When set, init copies squash via `pread` on the block device instead of opening the file on iso9660.
    pub fn getLiveSquashPread(self: *const Cmdline) ?LiveSquashPread {
        const v = self.getParam("dynamod.squash_pread") orelse return null;
        const colon = std.mem.indexOfScalar(u8, v, ':') orelse return null;
        if (colon == 0 or colon + 1 >= v.len) return null;
        const lba = std.fmt.parseInt(u64, v[0..colon], 10) catch return null;
        const size = std.fmt.parseInt(u64, v[colon + 1 ..], 10) catch return null;
        if (size == 0) return null;
        const sector: u64 = 2048;
        const start_byte = std.math.mul(u64, lba, sector) catch return null;
        return LiveSquashPread{ .start_byte = start_byte, .size = size };
    }

    /// Use overlayfs (tmpfs upper/work) on top of squashfs; default true when live is on.
    pub fn liveUseOverlay(self: *const Cmdline) bool {
        const v = self.getParam("dynamod.overlay") orelse return true;
        if (std.mem.eql(u8, v, "0") or std.mem.eql(u8, v, "false") or std.mem.eql(u8, v, "no"))
            return false;
        return true;
    }

    /// Loop-mount squashfs directly from the ISO (skips tmpfs copy). May hang on some kernels with iso9660 backing.
    pub fn liveDirectSquashFromIso(self: *const Cmdline) bool {
        const v = self.getParam("dynamod.live.direct_squash") orelse return false;
        return std.mem.eql(u8, v, "1") or
            std.mem.eql(u8, v, "true") or
            std.mem.eql(u8, v, "yes");
    }

    /// True when tmpfs copy of the squash image should be skipped (direct loop on ISO or legacy copy_squash=0).
    pub fn liveSkipTmpfsSquashCopy(self: *const Cmdline) bool {
        if (self.liveDirectSquashFromIso()) return true;
        const v = self.getParam("dynamod.live.copy_squash") orelse return false;
        return std.mem.eql(u8, v, "0") or std.mem.eql(u8, v, "false") or std.mem.eql(u8, v, "no");
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

test "parse dynamod live cmdline" {
    var cl = Cmdline{};
    const input = "dynamod.live=1 dynamod.media=LABEL=DYNAISO dynamod.squashfs=/live/root.squashfs dynamod.overlay=0";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expect(cl.isLive());
    try std.testing.expectEqualStrings("LABEL=DYNAISO", cl.getLiveMedia().?);
    try std.testing.expectEqualStrings("/live/root.squashfs", cl.getLiveSquashfsPath());
    try std.testing.expect(!cl.liveUseOverlay());
}

test "dynamod live bare flag and defaults" {
    var cl = Cmdline{};
    const input = "dynamod.live console=ttyS0";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expect(cl.isLive());
    try std.testing.expect(cl.getLiveMedia() == null);
    try std.testing.expectEqualStrings("/live/root.squashfs", cl.getLiveSquashfsPath());
    try std.testing.expect(cl.liveUseOverlay());
    try std.testing.expect(!cl.liveSkipTmpfsSquashCopy());
}

test "dynamod live copy_squash opt-out" {
    var cl = Cmdline{};
    const input = "dynamod.live=1 dynamod.live.copy_squash=0";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expect(cl.isLive());
    try std.testing.expect(cl.liveSkipTmpfsSquashCopy());
}

test "dynamod live direct_squash skips tmpfs copy" {
    var cl = Cmdline{};
    const input = "dynamod.live=1 dynamod.live.direct_squash=1";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    try std.testing.expect(cl.isLive());
    try std.testing.expect(cl.liveSkipTmpfsSquashCopy());
}

test "dynamod squash_pread parses LBA and size" {
    var cl = Cmdline{};
    const input = "dynamod.live=1 dynamod.squash_pread=18720:17698816";
    @memcpy(cl.buf[0..input.len], input);
    cl.len = input.len;

    const sp = cl.getLiveSquashPread().?;
    try std.testing.expectEqual(@as(u64, 18720 * 2048), sp.start_byte);
    try std.testing.expectEqual(@as(u64, 17698816), sp.size);
}
