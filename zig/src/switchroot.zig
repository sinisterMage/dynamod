/// Switch root implementation for initramfs-to-rootfs transition.
///
/// Mounts the real root filesystem, moves pseudo-filesystem mounts,
/// deletes initramfs contents to free RAM, and re-execs dynamod-init
/// from the real rootfs.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");
const cmdline = @import("cmdline.zig");
const rootdev = @import("rootdev.zig");

/// Perform the full initramfs -> rootfs transition. Does not return.
///
/// 1. Optionally run mdev -s for device node creation
/// 2. Resolve the root device
/// 3. Mount root device on /newroot
/// 4. Move /proc, /sys, /dev to /newroot
/// 5. Delete initramfs contents
/// 6. switch_root: chdir + MS_MOVE + chroot + execve
pub fn doSwitchRoot(cl: *const cmdline.Cmdline, klog_arg: ?kmsg) noreturn {
    const root_param = cl.getRoot() orelse {
        if (klog_arg) |k| k.emerg("no root= parameter on cmdline", .{});
        halt();
    };

    if (klog_arg) |k| k.info("initramfs: root={s}", .{root_param});

    // Run mdev -s to create device nodes (if mdev is available)
    runMdev(klog_arg);

    // Resolve root device
    const resolved = rootdev.resolve(root_param, cl.hasRootwait(), klog_arg) orelse {
        if (klog_arg) |k| k.emerg("failed to resolve root device: {s}", .{root_param});
        halt();
    };

    if (klog_arg) |k| k.info("root device: {s}", .{resolved.path()});

    // Optional rootdelay
    const delay = cl.getRootDelay();
    if (delay > 0) {
        if (klog_arg) |k| k.info("rootdelay={d}s", .{delay});
        const ts = linux.timespec{ .sec = @intCast(delay), .nsec = 0 };
        _ = linux.nanosleep(&ts, null);
    }

    // Create /newroot mountpoint
    _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), constants.newroot_path, 0o755);

    // Mount root device on /newroot
    mountRootDevice(resolved.path(), cl.getRootFsType(), cl.getRootFlags(), klog_arg) catch {
        if (klog_arg) |k| k.emerg("failed to mount root device on /newroot", .{});
        halt();
    };

    if (klog_arg) |k| k.info("mounted {s} on /newroot", .{resolved.path()});

    // Move pseudo-filesystem mounts to /newroot
    moveMounts(klog_arg);

    // Delete initramfs contents to free RAM
    if (klog_arg) |k| k.info("cleaning up initramfs...", .{});
    deleteInitramfsContents();

    // Switch root
    if (klog_arg) |k| k.info("switching root to /newroot", .{});
    execSwitchRoot(klog_arg);
}

/// Mount the root device on /newroot.
fn mountRootDevice(
    dev_path: []const u8,
    fstype_opt: ?[]const u8,
    flags_opt: ?[]const u8,
    klog_arg: ?kmsg,
) !void {
    // Build null-terminated device path
    var dev_z: [256:0]u8 = undefined;
    if (dev_path.len >= dev_z.len) return error.PathTooLong;
    @memcpy(dev_z[0..dev_path.len], dev_path);
    dev_z[dev_path.len] = 0;

    // Filesystem type (default: try auto-detect)
    var fstype_z: [32:0]u8 = undefined;
    const fstype_ptr: [*:0]const u8 = if (fstype_opt) |ft| blk: {
        if (ft.len >= fstype_z.len) return error.FsTypeTooLong;
        @memcpy(fstype_z[0..ft.len], ft);
        fstype_z[ft.len] = 0;
        break :blk @ptrCast(&fstype_z);
    } else "ext4"; // default to ext4

    const parsed = parseRootFlags(flags_opt);

    // Remaining (non-flag) mount options as null-terminated data string
    var data_z: [256:0]u8 = undefined;
    const data_ptr: usize = if (parsed.data_len > 0) blk: {
        @memcpy(data_z[0..parsed.data_len], parsed.data_buf[0..parsed.data_len]);
        data_z[parsed.data_len] = 0;
        break :blk @intFromPtr(@as([*:0]const u8, @ptrCast(&data_z)));
    } else 0;

    const rc = linux.mount(
        @ptrCast(&dev_z),
        constants.newroot_path,
        fstype_ptr,
        parsed.flags,
        data_ptr,
    );
    const e = linux.E.init(rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.err("mount failed: errno {d}", .{@intFromEnum(e)});
        return error.MountFailed;
    }
}

const MS_NOSUID_FLAG = 0x02;
const MS_NODEV_FLAG = 0x04;
const MS_NOEXEC_FLAG = 0x08;
const MS_SYNCHRONOUS = 0x10;
const MS_NOATIME = 0x400;
const MS_NODIRATIME = 0x800;
const MS_RELATIME = 0x200000;
const MS_STRICTATIME = 0x1000000;

const ParsedFlags = struct {
    flags: u32,
    data_buf: [256]u8,
    data_len: usize,
};

/// Parse rootflags like "rw,noatime,data=ordered" into mount flags and data.
/// Recognized flags become kernel mount flags; unrecognized options are passed
/// as the data string to mount(2) for the filesystem driver.
fn parseRootFlags(flags_opt: ?[]const u8) ParsedFlags {
    var result = ParsedFlags{
        .flags = constants.MS_RDONLY, // default: read-only
        .data_buf = undefined,
        .data_len = 0,
    };

    const flags_str = flags_opt orelse return result;

    var iter = std.mem.tokenizeScalar(u8, flags_str, ',');
    while (iter.next()) |opt| {
        if (std.mem.eql(u8, opt, "rw")) {
            result.flags &= ~constants.MS_RDONLY;
        } else if (std.mem.eql(u8, opt, "ro")) {
            result.flags |= constants.MS_RDONLY;
        } else if (std.mem.eql(u8, opt, "nosuid")) {
            result.flags |= MS_NOSUID_FLAG;
        } else if (std.mem.eql(u8, opt, "nodev")) {
            result.flags |= MS_NODEV_FLAG;
        } else if (std.mem.eql(u8, opt, "noexec")) {
            result.flags |= MS_NOEXEC_FLAG;
        } else if (std.mem.eql(u8, opt, "sync")) {
            result.flags |= MS_SYNCHRONOUS;
        } else if (std.mem.eql(u8, opt, "noatime")) {
            result.flags |= MS_NOATIME;
        } else if (std.mem.eql(u8, opt, "nodiratime")) {
            result.flags |= MS_NODIRATIME;
        } else if (std.mem.eql(u8, opt, "relatime")) {
            result.flags |= MS_RELATIME;
        } else if (std.mem.eql(u8, opt, "strictatime")) {
            result.flags |= MS_STRICTATIME;
        } else {
            // Unrecognized option: pass through as data string to fs driver
            if (result.data_len + opt.len + 1 < result.data_buf.len) {
                if (result.data_len > 0) {
                    result.data_buf[result.data_len] = ',';
                    result.data_len += 1;
                }
                @memcpy(result.data_buf[result.data_len..][0..opt.len], opt);
                result.data_len += opt.len;
            }
        }
    }

    return result;
}

/// Move /proc, /sys, /dev mounts into /newroot.
fn moveMounts(klog_arg: ?kmsg) void {
    const moves = [_]struct { from: [*:0]const u8, to: [*:0]const u8 }{
        .{ .from = "/proc", .to = "/newroot/proc" },
        .{ .from = "/sys", .to = "/newroot/sys" },
        .{ .from = "/dev", .to = "/newroot/dev" },
    };

    for (&moves) |m| {
        // Create target mountpoint
        _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), m.to, 0o755);

        const rc = linux.mount(m.from, m.to, @ptrFromInt(0), constants.MS_MOVE, 0);
        const e = linux.E.init(rc);
        if (e != .SUCCESS) {
            if (klog_arg) |k| {
                const from_span = std.mem.span(m.from);
                k.warn("failed to move {s}: errno {d}", .{ from_span, @intFromEnum(e) });
            }
        } else {
            if (klog_arg) |k| {
                const from_span = std.mem.span(m.from);
                const to_span = std.mem.span(m.to);
                k.info("moved {s} -> {s}", .{ from_span, to_span });
            }
        }
    }
}

/// Delete initramfs contents to free RAM.
/// Skips /newroot (the mounted real root) and any other active mount points.
fn deleteInitramfsContents() void {
    // Open the root directory
    const root_fd = linux.open("/", .{ .ACCMODE = .RDONLY, .DIRECTORY = true }, 0);
    const e = linux.E.init(root_fd);
    if (e != .SUCCESS) return;

    // Get the device ID of rootfs for mount point detection
    var root_stat: linux.Stat = undefined;
    const stat_rc = linux.fstat(@intCast(root_fd), &root_stat);
    if (linux.E.init(stat_rc) != .SUCCESS) {
        _ = linux.close(@intCast(root_fd));
        return;
    }

    recursiveDelete(@intCast(root_fd), root_stat.dev, 0);
    _ = linux.close(@intCast(root_fd));
}

/// Recursively delete directory contents, skipping mount points.
fn recursiveDelete(dir_fd: std.posix.fd_t, root_dev: u64, depth: u32) void {
    if (depth > 16) return; // prevent stack overflow

    var buf: [4096]u8 = undefined;

    while (true) {
        const nread = linux.getdents64(dir_fd, &buf, buf.len);
        const nread_e = linux.E.init(nread);
        if (nread_e != .SUCCESS or nread == 0) break;

        var offset: usize = 0;
        while (offset < nread) {
            const entry: *align(1) linux.dirent64 = @ptrCast(@alignCast(&buf[offset]));
            offset += entry.reclen;

            const name_ptr: [*:0]const u8 = @ptrCast(&entry.name);
            const name = std.mem.span(name_ptr);

            // Skip . and ..
            if (std.mem.eql(u8, name, ".") or std.mem.eql(u8, name, "..")) continue;

            // Skip /newroot
            if (depth == 0 and std.mem.eql(u8, name, "newroot")) continue;

            // Check if this is a mount point (different device)
            var child_stat: linux.Stat = undefined;
            const sr = linux.fstatat(
                dir_fd,
                name_ptr,
                &child_stat,
                linux.AT.SYMLINK_NOFOLLOW,
            );
            if (linux.E.init(sr) != .SUCCESS) continue;

            if (child_stat.dev != root_dev) continue; // mount point, skip

            if (entry.type == linux.DT.DIR) {
                // Recurse into directory
                const child_fd = linux.openat(
                    dir_fd,
                    name_ptr,
                    .{ .ACCMODE = .RDONLY, .DIRECTORY = true },
                    0,
                );
                if (linux.E.init(child_fd) == .SUCCESS) {
                    recursiveDelete(@intCast(child_fd), root_dev, depth + 1);
                    _ = linux.close(@intCast(child_fd));
                }
                // Remove the now-empty directory
                _ = linux.unlinkat(dir_fd, name_ptr, linux.AT.REMOVEDIR);
            } else {
                // Remove file/symlink
                _ = linux.unlinkat(dir_fd, name_ptr, 0);
            }
        }
    }
}

/// Execute the switch_root sequence: chdir, MS_MOVE, chroot, execve.
fn execSwitchRoot(klog_arg: ?kmsg) noreturn {
    // chdir("/newroot")
    var chdir_rc = linux.chdir(constants.newroot_path);
    var e = linux.E.init(chdir_rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.emerg("chdir /newroot failed: errno {d}", .{@intFromEnum(e)});
        halt();
    }

    // mount(".", "/", NULL, MS_MOVE, 0) — move /newroot to /
    const dot: [*:0]const u8 = ".";
    const slash: [*:0]const u8 = "/";
    const mv_rc = linux.mount(dot, slash, @ptrFromInt(0), constants.MS_MOVE, 0);
    e = linux.E.init(mv_rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.emerg("mount MS_MOVE failed: errno {d}", .{@intFromEnum(e)});
        halt();
    }

    // chroot(".")
    const chroot_rc = linux.syscall1(.chroot, @intFromPtr(dot));
    e = linux.E.init(chroot_rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.emerg("chroot failed: errno {d}", .{@intFromEnum(e)});
        halt();
    }

    // chdir("/") — reset cwd to new root
    chdir_rc = linux.chdir(slash);
    e = linux.E.init(chdir_rc);
    if (e != .SUCCESS) {
        if (klog_arg) |k| k.emerg("chdir / failed: errno {d}", .{@intFromEnum(e)});
        halt();
    }

    if (klog_arg) |k| k.info("executing {s} from real rootfs", .{std.mem.span(constants.init_path)});

    // Re-exec dynamod-init from the real rootfs
    const argv = [_:null]?[*:0]const u8{constants.init_path};
    const envp = [_:null]?[*:0]const u8{};
    _ = linux.execve(constants.init_path, &argv, &envp);

    // If execve failed, we're in trouble
    if (klog_arg) |k| k.emerg("execve failed!", .{});
    halt();
}

/// Fork and exec mdev -s to create device nodes in /dev.
/// This is needed for UUID/LABEL resolution via /dev/disk/by-*/ symlinks.
fn runMdev(klog_arg: ?kmsg) void {
    // Check if mdev exists
    _ = std.fs.openFileAbsolute("/sbin/mdev", .{}) catch {
        if (klog_arg) |k| k.info("mdev not found, skipping device scan", .{});
        return;
    };

    if (klog_arg) |k| k.info("running mdev -s for device enumeration", .{});

    const pid_rc = linux.fork();
    const pid_e = linux.E.init(pid_rc);
    if (pid_e != .SUCCESS) {
        if (klog_arg) |k| k.warn("fork for mdev failed", .{});
        return;
    }

    if (pid_rc == 0) {
        // Child: exec mdev -s
        const mdev_argv = [_:null]?[*:0]const u8{ constants.mdev_path, "-s" };
        const mdev_envp = [_:null]?[*:0]const u8{};
        _ = linux.execve(constants.mdev_path, &mdev_argv, &mdev_envp);
        // If exec fails, exit child
        linux.exit(1);
    }

    // Parent: wait for mdev to finish
    var status: u32 = 0;
    _ = linux.wait4(@intCast(pid_rc), &status, 0, null);
    if (klog_arg) |k| k.info("mdev completed", .{});
}

/// Emergency halt — called when switch_root fails irrecoverably.
fn halt() noreturn {
    // Try to sync before halting
    _ = linux.sync();
    while (true) {
        _ = linux.nanosleep(&linux.timespec{ .sec = 3600, .nsec = 0 }, null);
    }
}
