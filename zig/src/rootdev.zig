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
/// - `LABEL=xxxx` → `/dev/disk/by-label/` symlink (exact or case-insensitive), then `blkid -L`
///   (minimal initramfs mdev often has no by-label links; blkid finds ISO9660 volume labels).
///
/// If `rootwait` is true, polls every 250ms until the device appears
/// (up to rootwait_max_s seconds).
pub fn resolve(root_param: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    if (root_param.len == 0) return null;

    // Direct /dev/ path
    if (std.mem.startsWith(u8, root_param, "/dev/")) {
        return resolveDirectPath(root_param, rootwait, klog_arg);
    }

    if (std.mem.startsWith(u8, root_param, "LABEL=")) {
        const value = root_param["LABEL=".len..];
        return resolveLabelValue(value, rootwait, klog_arg);
    }

    if (std.mem.startsWith(u8, root_param, "UUID=")) {
        const value = root_param["UUID=".len..];
        return resolveUuidValue(value, rootwait, klog_arg);
    }

    // PARTUUID= resolution via /dev/disk/by-partuuid/ symlinks
    if (std.mem.startsWith(u8, root_param, "PARTUUID=")) {
        const value = root_param["PARTUUID=".len..];
        return resolveSymlink("/dev/disk/by-partuuid", value, rootwait, klog_arg);
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

const by_uuid_dir = "/dev/disk/by-uuid";

/// UUID=: udev-style symlinks first, then `blkid -U` fallback for minimal
/// initramfs environments without udev/mdev.
fn resolveUuidValue(value: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    const max_attempts = if (rootwait)
        (constants.rootwait_max_s * 1000) / constants.rootwait_poll_ms
    else
        @as(u32, 1);

    var attempt: u32 = 0;
    while (attempt < max_attempts) : (attempt += 1) {
        if (attempt > 0) {
            const ts = linux.timespec{
                .sec = 0,
                .nsec = @as(i64, constants.rootwait_poll_ms) * 1_000_000,
            };
            _ = linux.nanosleep(&ts, null);
        }

        // Try /dev/disk/by-uuid/<uuid> symlink (works when udev is running)
        {
            var link_path_buf: [512]u8 = undefined;
            const link_path = std.fmt.bufPrint(&link_path_buf, "{s}/{s}", .{ by_uuid_dir, value }) catch continue;
            const file = std.fs.openFileAbsolute(link_path, .{}) catch {
                // Symlink not found — try blkid fallback
                if (tryBlkidUuidDevice(value, klog_arg)) |r| {
                    if (klog_arg) |k| k.info("resolved UUID={s} via blkid -> {s}", .{ value, r.path() });
                    return r;
                }
                continue;
            };
            file.close();

            var result = ResolvedDevice{};
            if (link_path.len > result.path_buf.len - 1) continue;
            @memcpy(result.path_buf[0..link_path.len], link_path);
            result.path_buf[link_path.len] = 0;
            result.path_len = link_path.len;

            if (klog_arg) |k| k.info("resolved UUID={s} -> {s}", .{ value, result.path() });
            return result;
        }
    }

    if (rootwait) {
        if (klog_arg) |k| k.err("timed out waiting for {s} in {s}", .{ value, by_uuid_dir });
    } else {
        if (klog_arg) |k| k.err("UUID={s} not found", .{value});
    }
    return null;
}

/// Try to resolve a UUID by running `blkid` (no flags — busybox compatible)
/// and scanning the output for a line containing UUID="<value>".
/// Output format: /dev/vda2: UUID="xxxx" TYPE="ext4" ...
fn tryBlkidUuidDevice(uuid: []const u8, _: ?kmsg) ?ResolvedDevice {
    _ = std.fs.openFileAbsolute(std.mem.span(blkid_path), .{}) catch return null;

    const pid_rc = linux.fork();
    const pid_e = linux.E.init(pid_rc);
    if (pid_e != .SUCCESS) return null;

    if (pid_rc == 0) {
        const out: [*:0]const u8 = "/run/dynamod-blkid-out";
        const out_fd = linux.open(out, .{ .ACCMODE = .WRONLY, .CREAT = true, .TRUNC = true }, 0o644);
        if (linux.E.init(out_fd) != .SUCCESS) linux.exit(127);
        _ = linux.dup2(@intCast(out_fd), 1);
        _ = linux.close(@intCast(out_fd));
        const null_fd = linux.open("/dev/null", .{ .ACCMODE = .WRONLY }, 0);
        if (linux.E.init(null_fd) == .SUCCESS) {
            _ = linux.dup2(@intCast(null_fd), 2);
            _ = linux.close(@intCast(null_fd));
        }
        const argv = [_:null]?[*:0]const u8{blkid_path};
        const envp = [_:null]?[*:0]const u8{};
        _ = linux.execve(blkid_path, &argv, &envp);
        linux.exit(127);
    }

    var status: u32 = 0;
    _ = linux.wait4(@intCast(pid_rc), &status, 0, null);

    const file = std.fs.openFileAbsolute(blkid_out_path, .{}) catch return null;
    defer file.close();
    var read_buf: [4096]u8 = undefined;
    const n = file.readAll(&read_buf) catch return null;
    if (n == 0) return null;

    // Build needle: UUID="<uuid>"
    var needle_buf: [300]u8 = undefined;
    const needle = std.fmt.bufPrint(&needle_buf, "UUID=\"{s}\"", .{uuid}) catch return null;

    // Scan line-by-line for the matching UUID
    var pos: usize = 0;
    while (pos < n) {
        var end = pos;
        while (end < n and read_buf[end] != '\n') : (end += 1) {}
        const line = read_buf[pos..end];

        if (std.mem.indexOf(u8, line, needle) != null) {
            // Extract device path: everything before first ':'
            if (std.mem.indexOfScalar(u8, line, ':')) |colon| {
                const dp = std.mem.trim(u8, line[0..colon], " \t");
                if (dp.len >= 5 and std.mem.startsWith(u8, dp, "/dev/") and deviceExists(dp)) {
                    var result = ResolvedDevice{};
                    if (dp.len > result.path_buf.len - 1) return null;
                    @memcpy(result.path_buf[0..dp.len], dp);
                    result.path_buf[dp.len] = 0;
                    result.path_len = dp.len;
                    return result;
                }
            }
        }

        pos = if (end < n) end + 1 else n;
    }

    return null;
}

const by_label_dir = "/dev/disk/by-label";
const blkid_path: [*:0]const u8 = "/bin/blkid";
const blkid_out_path = "/run/dynamod-blkid-out";

/// LABEL=: udev-style symlinks first, then `blkid -L` (works for ISO volume id when by-label is missing).
fn resolveLabelValue(value: []const u8, rootwait: bool, klog_arg: ?kmsg) ?ResolvedDevice {
    const max_attempts = if (rootwait)
        (constants.rootwait_max_s * 1000) / constants.rootwait_poll_ms
    else
        @as(u32, 1);

    var attempt: u32 = 0;
    while (attempt < max_attempts) : (attempt += 1) {
        if (attempt > 0) {
            const ts = linux.timespec{
                .sec = 0,
                .nsec = @as(i64, constants.rootwait_poll_ms) * 1_000_000,
            };
            _ = linux.nanosleep(&ts, null);
        }

        if (tryResolveLabelSymlinkExact(value)) |r| {
            if (klog_arg) |k| k.info("resolved LABEL={s} -> {s}", .{ value, r.path() });
            return r;
        }
        if (tryResolveLabelSymlinkScan(value)) |r| {
            if (klog_arg) |k| k.info("resolved LABEL={s} (by-label scan) -> {s}", .{ value, r.path() });
            return r;
        }
        if (tryBlkidLabelDevice(value, klog_arg)) |r| {
            if (klog_arg) |k| k.info("resolved LABEL={s} via blkid -> {s}", .{ value, r.path() });
            return r;
        }
    }

    if (rootwait) {
        if (klog_arg) |k| k.err("timed out waiting for LABEL={s} (by-label and blkid)", .{value});
    } else {
        if (klog_arg) |k| k.err("LABEL={s} not found", .{value});
    }
    return null;
}

fn tryResolveLabelSymlinkExact(value: []const u8) ?ResolvedDevice {
    var link_path_buf: [512]u8 = undefined;
    const link_path = std.fmt.bufPrint(&link_path_buf, "{s}/{s}", .{ by_label_dir, value }) catch return null;
    const f = std.fs.openFileAbsolute(link_path, .{}) catch return null;
    defer f.close();
    var result = ResolvedDevice{};
    if (link_path.len > result.path_buf.len - 1) return null;
    @memcpy(result.path_buf[0..link_path.len], link_path);
    result.path_buf[link_path.len] = 0;
    result.path_len = link_path.len;
    return result;
}

fn tryResolveLabelSymlinkScan(value: []const u8) ?ResolvedDevice {
    var dir = std.fs.openDirAbsolute(by_label_dir, .{ .iterate = true }) catch return null;
    defer dir.close();
    var it = dir.iterate();
    while (true) {
        const entry_opt = it.next() catch return null;
        const entry = entry_opt orelse break;
        if (!std.ascii.eqlIgnoreCase(entry.name, value)) continue;
        var link_path_buf: [512]u8 = undefined;
        const link_path = std.fmt.bufPrint(&link_path_buf, "{s}/{s}", .{ by_label_dir, entry.name }) catch continue;
        const f = std.fs.openFileAbsolute(link_path, .{}) catch continue;
        f.close();
        var result = ResolvedDevice{};
        if (link_path.len > result.path_buf.len - 1) continue;
        @memcpy(result.path_buf[0..link_path.len], link_path);
        result.path_buf[link_path.len] = 0;
        result.path_len = link_path.len;
        return result;
    }
    return null;
}

/// Parse blkid stdout: device-only line or `device: attrs...`.
fn parseBlkidDeviceLine(raw: []const u8) ?[]const u8 {
    const line = std.mem.trim(u8, raw, " \t\n\r");
    if (line.len == 0) return null;
    const dev_part = if (std.mem.indexOfScalar(u8, line, ':')) |i| line[0..i] else line;
    const dev = std.mem.trim(u8, dev_part, " \t");
    if (dev.len < 6 or !std.mem.startsWith(u8, dev, "/dev/")) return null;
    return dev;
}

fn tryBlkidLabelDevice(label: []const u8, _: ?kmsg) ?ResolvedDevice {
    _ = std.fs.openFileAbsolute(std.mem.span(blkid_path), .{}) catch return null;

    var label_z: [256:0]u8 = undefined;
    if (label.len >= label_z.len - 1) return null;
    @memcpy(label_z[0..label.len], label);
    label_z[label.len] = 0;

    const pid_rc = linux.fork();
    const pid_e = linux.E.init(pid_rc);
    if (pid_e != .SUCCESS) return null;

    if (pid_rc == 0) {
        const out: [*:0]const u8 = "/run/dynamod-blkid-out";
        const out_fd = linux.open(out, .{ .ACCMODE = .WRONLY, .CREAT = true, .TRUNC = true }, 0o644);
        if (linux.E.init(out_fd) != .SUCCESS) linux.exit(127);
        _ = linux.dup2(@intCast(out_fd), 1);
        _ = linux.close(@intCast(out_fd));
        const null_fd = linux.open("/dev/null", .{ .ACCMODE = .WRONLY }, 0);
        if (linux.E.init(null_fd) == .SUCCESS) {
            _ = linux.dup2(@intCast(null_fd), 2);
            _ = linux.close(@intCast(null_fd));
        }
        const arg_l: [*:0]const u8 = "-L";
        const argv = [_:null]?[*:0]const u8{ blkid_path, arg_l, @ptrCast(&label_z) };
        const envp = [_:null]?[*:0]const u8{};
        _ = linux.execve(blkid_path, &argv, &envp);
        linux.exit(127);
    }

    var status: u32 = 0;
    _ = linux.wait4(@intCast(pid_rc), &status, 0, null);
    // Normal exit: low byte 0; exit code in bits 8–15 (Linux wait status).
    if ((status & 0xff) != 0) return null;
    if (((status >> 8) & 0xff) != 0) return null;

    const file = std.fs.openFileAbsolute(blkid_out_path, .{}) catch return null;
    defer file.close();
    var read_buf: [300]u8 = undefined;
    const n = file.readAll(&read_buf) catch return null;
    const dev_path = parseBlkidDeviceLine(read_buf[0..n]) orelse return null;
    if (!deviceExists(dev_path)) return null;

    var result = ResolvedDevice{};
    if (dev_path.len > result.path_buf.len - 1) return null;
    @memcpy(result.path_buf[0..dev_path.len], dev_path);
    result.path_buf[dev_path.len] = 0;
    result.path_len = dev_path.len;
    return result;
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
