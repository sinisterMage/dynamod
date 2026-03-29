/// Early boot operations: mount pseudo-filesystems, set hostname, seed entropy.
/// These must run before anything else in userspace.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

const MountEntry = struct {
    source: [*:0]const u8,
    target: [*:0]const u8,
    fstype: [*:0]const u8,
    flags: u32,
    data: ?[*:0]const u8,
};

const MS_NOSUID = 0x02;
const MS_NODEV = 0x04;
const MS_NOEXEC = 0x08;
const MS_STRICTATIME = 0x1000000;

const essential_mounts = [_]MountEntry{
    .{ .source = "proc", .target = "/proc", .fstype = "proc", .flags = MS_NOSUID | MS_NODEV | MS_NOEXEC, .data = null },
    .{ .source = "sysfs", .target = "/sys", .fstype = "sysfs", .flags = MS_NOSUID | MS_NODEV | MS_NOEXEC, .data = null },
    .{ .source = "devtmpfs", .target = "/dev", .fstype = "devtmpfs", .flags = MS_NOSUID | MS_STRICTATIME, .data = "mode=0755" },
    .{ .source = "devpts", .target = "/dev/pts", .fstype = "devpts", .flags = MS_NOSUID | MS_NOEXEC, .data = "mode=0620,gid=5" },
    .{ .source = "tmpfs", .target = "/dev/shm", .fstype = "tmpfs", .flags = MS_NOSUID | MS_NODEV, .data = null },
    .{ .source = "tmpfs", .target = "/run", .fstype = "tmpfs", .flags = MS_NOSUID | MS_NODEV | MS_STRICTATIME, .data = "mode=0755" },
    .{ .source = "cgroup2", .target = "/sys/fs/cgroup", .fstype = "cgroup2", .flags = MS_NOSUID | MS_NODEV | MS_NOEXEC, .data = null },
};

/// Mount all essential pseudo-filesystems needed for early boot.
pub fn mountEssentialFilesystems(klog_arg: ?kmsg) void {
    for (&essential_mounts) |*entry| {
        const target = std.mem.span(entry.target);

        // Create mount point if it doesn't exist (use raw syscall for sentinel ptr)
        _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), entry.target, 0o755);

        const data: usize = if (entry.data) |d| @intFromPtr(d) else 0;
        const rc = linux.mount(entry.source, entry.target, entry.fstype, entry.flags, data);
        const e = linux.E.init(rc);
        if (e != .SUCCESS and e != .BUSY) {
            if (klog_arg) |k| {
                k.err("failed to mount {s}: errno {d}", .{ target, @intFromEnum(e) });
            }
        } else {
            if (klog_arg) |k| {
                k.info("mounted {s}", .{target});
            }
        }
    }
}

/// Create the /run/dynamod runtime directory.
pub fn createRuntimeDir(klog_arg: ?kmsg) void {
    const rc = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), constants.run_dir, 0o755);
    const e = linux.E.init(rc);
    if (e != .SUCCESS and e != .EXIST) {
        if (klog_arg) |k| {
            k.err("failed to create {s}: errno {d}", .{ constants.run_dir, @intFromEnum(e) });
        }
    }
}

/// Set the system hostname from /etc/hostname.
pub fn setHostname(klog_arg: ?kmsg) void {
    var buf: [256]u8 = undefined;
    const file = std.fs.openFileAbsolute(constants.hostname_path, .{}) catch {
        if (klog_arg) |k| k.info("no {s} found, skipping hostname", .{constants.hostname_path});
        return;
    };
    defer file.close();

    const len = file.read(&buf) catch {
        if (klog_arg) |k| k.warn("failed to read {s}", .{constants.hostname_path});
        return;
    };

    // Trim trailing whitespace/newlines
    var hostname = buf[0..len];
    while (hostname.len > 0 and (hostname[hostname.len - 1] == '\n' or hostname[hostname.len - 1] == '\r' or hostname[hostname.len - 1] == ' ')) {
        hostname = hostname[0 .. hostname.len - 1];
    }

    if (hostname.len == 0) return;

    // Use raw sethostname syscall
    const rc = linux.syscall2(.sethostname, @intFromPtr(hostname.ptr), hostname.len);
    const e = linux.E.init(rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.warn("failed to set hostname: errno {d}", .{@intFromEnum(e)});
    } else {
        if (klog_arg) |k| k.info("hostname set to '{s}'", .{hostname});
    }
}

/// Generate /etc/machine-id if it does not exist.
/// The machine-id is a 32-character lowercase hex string followed by a newline.
/// Many D-Bus consumers and desktop tools expect this file to exist.
pub fn ensureMachineId(klog_arg: ?kmsg) void {
    // Check if machine-id already exists and has content
    if (std.fs.openFileAbsolute(constants.machine_id_path, .{})) |file| {
        defer file.close();
        var buf: [33]u8 = undefined;
        const len = file.read(&buf) catch 0;
        if (len >= 32) return; // Already has a valid machine-id
    } else |_| {}

    // Generate 16 random bytes from /dev/urandom
    const urandom = std.fs.openFileAbsolute("/dev/urandom", .{}) catch {
        if (klog_arg) |k| k.warn("cannot open /dev/urandom for machine-id", .{});
        return;
    };
    defer urandom.close();

    var random_bytes: [16]u8 = undefined;
    _ = urandom.read(&random_bytes) catch {
        if (klog_arg) |k| k.warn("cannot read /dev/urandom for machine-id", .{});
        return;
    };

    // Convert to 32 hex characters + newline
    const hex_chars = "0123456789abcdef";
    var hex_buf: [33]u8 = undefined;
    for (random_bytes, 0..) |byte, i| {
        hex_buf[i * 2] = hex_chars[byte >> 4];
        hex_buf[i * 2 + 1] = hex_chars[byte & 0x0f];
    }
    hex_buf[32] = '\n';

    // Write machine-id file
    const file = std.fs.createFileAbsolute(constants.machine_id_path, .{}) catch {
        if (klog_arg) |k| k.warn("cannot create {s}", .{constants.machine_id_path});
        return;
    };
    defer file.close();

    _ = file.write(&hex_buf) catch {
        if (klog_arg) |k| k.warn("cannot write {s}", .{constants.machine_id_path});
        return;
    };

    if (klog_arg) |k| k.info("generated {s}", .{constants.machine_id_path});
}

/// Detect if we're running in an initramfs (rootfs or tmpfs).
/// Returns true if the root filesystem is RAMFS or TMPFS, indicating
/// we're in an initramfs and may need to switch_root to a real rootfs.
pub fn isInitramfs() bool {
    // Use raw syscall since Zig std doesn't expose a statfs struct.
    // statfs64 struct layout for x86_64: f_type is the first i64 field.
    var buf: [120]u8 = undefined; // statfs64 is ~120 bytes on x86_64
    const root: [*:0]const u8 = "/";
    const rc = linux.syscall2(.statfs, @intFromPtr(root), @intFromPtr(&buf));
    if (linux.E.init(rc) != .SUCCESS) return false;
    // f_type is the first field, an i64 (8 bytes) on x86_64
    const f_type = std.mem.readInt(i64, buf[0..8], .little);
    return f_type == constants.RAMFS_MAGIC or f_type == constants.TMPFS_MAGIC;
}

/// Seed the kernel PRNG from saved random seed.
pub fn seedEntropy(klog_arg: ?kmsg) void {
    const seed_file = std.fs.openFileAbsolute(constants.random_seed_path, .{}) catch return;
    defer seed_file.close();

    var buf: [512]u8 = undefined;
    const len = seed_file.read(&buf) catch return;
    if (len == 0) return;

    const urandom = std.fs.openFileAbsolute("/dev/urandom", .{ .mode = .write_only }) catch return;
    defer urandom.close();
    _ = urandom.write(buf[0..len]) catch {};

    if (klog_arg) |k| k.info("seeded entropy from {s} ({d} bytes)", .{ constants.random_seed_path, len });
}
