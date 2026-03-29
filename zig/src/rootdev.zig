/// Root device resolver for initramfs boot.
///
/// Resolves the `root=` kernel parameter to an actual device path.
/// Supports: /dev/XXX (direct), UUID=, PARTUUID=, LABEL= (via symlinks).
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

pub const ResolvedDevice = struct {
    /// Buffer holding the resolved device path.
    path_buf: [256]u8 = undefined,
    path_len: usize = 0,

    pub fn path(self: *const ResolvedDevice) []const u8 {
        return self.path_buf[0..self.path_len];
    }

    /// Return the path as a sentinel-terminated pointer for syscalls.
    pub fn pathZ(self: *const ResolvedDevice) [*:0]const u8 {
        // Ensure null termination
        return @ptrCast(self.path_buf[0..self.path_len :0]);
    }
};

/// Resolve a root= parameter to a device path.
///
/// - `/dev/XXX` → use directly
/// - `UUID=xxxx` → scan /dev/disk/by-uuid/
/// - `PARTUUID=xxxx` → scan /dev/disk/by-partuuid/
/// - `LABEL=xxxx` → scan /dev/disk/by-label/
///
/// If `rootwait` is true, polls every 250ms until the device appears
/// (up to rootwait_max_s seconds).
pub fn resolve(root_param: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    if (root_param.len == 0) return null;

    // Direct /dev/ path
    if (std.mem.startsWith(u8, root_param, "/dev/")) {
        return resolveDirectPath(root_param, rootwait, klog_arg);
    }

    // UUID=, PARTUUID=, LABEL= resolution via /dev/disk/by-*/ symlinks
    const SymlinkDir = struct { prefix: []const u8, dir: []const u8 };
    const mappings = [_]SymlinkDir{
        .{ .prefix = "UUID=", .dir = "/dev/disk/by-uuid" },
        .{ .prefix = "PARTUUID=", .dir = "/dev/disk/by-partuuid" },
        .{ .prefix = "LABEL=", .dir = "/dev/disk/by-label" },
    };

    for (&mappings) |m| {
        if (std.mem.startsWith(u8, root_param, m.prefix)) {
            const value = root_param[m.prefix.len..];
            return resolveSymlink(m.dir, value, rootwait, klog_arg);
        }
    }

    // Unknown format — try as a direct path anyway
    if (klog_arg) |k| k.warn("unknown root= format: {s}, trying as path", .{root_param});
    return resolveDirectPath(root_param, rootwait, klog_arg);
}

/// Check if a direct device path exists, optionally waiting for it.
fn resolveDirectPath(dev_path: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    var result = ResolvedDevice{};
    if (dev_path.len > result.path_buf.len - 1) return null;

    @memcpy(result.path_buf[0..dev_path.len], dev_path);
    result.path_buf[dev_path.len] = 0; // null terminate
    result.path_len = dev_path.len;

    if (deviceExists(dev_path)) return result;

    if (rootwait) {
        if (klog_arg) |k| k.info("waiting for device {s}...", .{dev_path});
        return waitForDevice(result, klog_arg);
    }

    if (klog_arg) |k| k.err("root device not found: {s}", .{dev_path});
    return null;
}

/// Resolve a UUID/PARTUUID/LABEL by scanning a /dev/disk/by-*/ directory.
fn resolveSymlink(dir: []const u8, value: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    const max_attempts = if (rootwait)
        (constants.rootwait_max_s * 1000) / constants.rootwait_poll_ms
    else
        @as(u32, 1);

    var attempt: u32 = 0;
    while (attempt < max_attempts) : (attempt += 1) {
        if (attempt > 0) {
            // Sleep rootwait_poll_ms milliseconds
            const ts = linux.timespec{
                .sec = 0,
                .nsec = @as(i64, constants.rootwait_poll_ms) * 1_000_000,
            };
            _ = linux.nanosleep(&ts, null);
        }

        // Build the symlink path: dir/value
        var link_path_buf: [512]u8 = undefined;
        const link_path = std.fmt.bufPrint(&link_path_buf, "{s}/{s}", .{ dir, value }) catch continue;

        // Check if the symlink path exists
        const file = std.fs.openFileAbsolute(link_path, .{}) catch continue;
        file.close();

        // The symlink exists — now readlink to get the actual device path
        // Use the absolute path approach: the symlinks are like ../../sda1
        // We want the canonical path, so just use the link_path as-is and
        // let the mount syscall resolve it
        var result = ResolvedDevice{};
        if (link_path.len > result.path_buf.len - 1) continue;
        @memcpy(result.path_buf[0..link_path.len], link_path);
        result.path_buf[link_path.len] = 0;
        result.path_len = link_path.len;

        if (klog_arg) |k| k.info("resolved {s}{s} -> {s}", .{ dir, value, result.path() });
        return result;
    }

    if (rootwait) {
        if (klog_arg) |k| k.err("timed out waiting for {s} in {s}", .{ value, dir });
    } else {
        if (klog_arg) |k| k.err("not found: {s}/{s}", .{ dir, value });
    }
    return null;
}

/// Wait for a device to appear, polling every rootwait_poll_ms.
fn waitForDevice(result: ResolvedDevice, klog_arg: ?kmsg) ?ResolvedDevice {
    const max_attempts = (constants.rootwait_max_s * 1000) / constants.rootwait_poll_ms;
    var attempt: u32 = 0;
    while (attempt < max_attempts) : (attempt += 1) {
        if (deviceExists(result.path())) return result;
        const ts = linux.timespec{
            .sec = 0,
            .nsec = @as(i64, constants.rootwait_poll_ms) * 1_000_000,
        };
        _ = linux.nanosleep(&ts, null);
    }
    if (klog_arg) |k| k.err("timed out waiting for {s}", .{result.path()});
    return null;
}

/// Check if a device path exists by attempting to stat it.
fn deviceExists(path: []const u8) bool {
    _ = std.fs.openFileAbsolute(path, .{}) catch return false;
    // File exists (we opened it successfully). Close implicitly via defer-less pattern.
    // Actually, we need to close it. Use a different approach:
    const file = std.fs.openFileAbsolute(path, .{}) catch return false;
    file.close();
    return true;
}

// --- Tests ---

test "direct path resolution" {
    // Can't test actual device presence in unit tests, but we can test the logic
    const result = resolve("/dev/nonexistent", false, null);
    try std.testing.expect(result == null);
}

test "unknown format treated as path" {
    const result = resolve("nonexistent", false, null);
    try std.testing.expect(result == null);
}
