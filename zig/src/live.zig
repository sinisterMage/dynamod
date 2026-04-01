/// ISO / live boot: mount boot media (iso9660/udf), loop-mount squashfs, optional overlayfs, then switch_root.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");
const cmdline = @import("cmdline.zig");
const rootdev = @import("rootdev.zig");
const switchroot = @import("switchroot.zig");

/// linux/loop.h
const LOOP_SET_FD: u32 = 0x4C00;
const LOOP_CTL_GET_FREE: u32 = 0x4C82;

const MS_BIND: u32 = 0x1000;

fn halt() noreturn {
    _ = linux.sync();
    while (true) {
        _ = linux.nanosleep(&linux.timespec{ .sec = 3600, .nsec = 0 }, null);
    }
}

fn kernelModuleTreePresent() bool {
    var d = std.fs.openDirAbsolute("/lib/modules", .{}) catch return false;
    defer d.close();
    return true;
}

/// Best-effort modprobe for modular kernels (ignores failure if busybox/kmod missing).
fn tryModprobe(name: [*:0]const u8) void {
    const modprobe: [*:0]const u8 = "/bin/modprobe";
    _ = std.fs.openFileAbsolute(std.mem.span(modprobe), .{}) catch return;

    const pid_rc = linux.fork();
    const pid_e = linux.E.init(pid_rc);
    if (pid_e != .SUCCESS) return;

    if (pid_rc == 0) {
        const argv = [_:null]?[*:0]const u8{ modprobe, name };
        const envp = [_:null]?[*:0]const u8{};
        _ = linux.execve(modprobe, &argv, &envp);
        linux.exit(1);
    }

    var status: u32 = 0;
    _ = linux.wait4(@intCast(pid_rc), &status, 0, null);
}

/// CD/SCSI block devices (e.g. /dev/sr0) must be probed before rootdev.resolve — modular
/// kernels otherwise never create the node during rootwait.
fn loadLiveMediaModules(klog_arg: ?kmsg) void {
    if (!kernelModuleTreePresent()) {
        if (klog_arg) |k| k.warn("dynamod live: no /lib/modules — modprobe skipped; use built-in sr/ata/squashfs or bundle modules into initramfs", .{});
        return;
    }
    const names = [_][*:0]const u8{
        "scsi_mod",
        "ata_piix",
        "cdrom",
        "sr_mod",
        "virtio_scsi",
    };
    if (klog_arg) |k| k.info("dynamod live: loading block/media modules (best-effort)", .{});
    for (names) |n| tryModprobe(n);
}

fn loadLiveFsModules(klog_arg: ?kmsg) void {
    if (!kernelModuleTreePresent()) {
        if (klog_arg) |k| k.warn("dynamod live: no /lib/modules — FS modprobe skipped; use built-in iso9660/squashfs/overlay/loop", .{});
        return;
    }
    const names = [_][*:0]const u8{
        "loop",
        "squashfs",
        "iso9660",
        "udf",
        "overlay",
    };
    if (klog_arg) |k| k.info("dynamod live: loading filesystem modules (best-effort)", .{});
    for (names) |n| tryModprobe(n);
}

fn mkdir_path(path: [*:0]const u8) void {
    _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), path, 0o755);
}

/// `inner_abs` must start with `/` (path on the ISO). Result: `iso_base` + inner without duplicate slash.
fn joinIsoPath(out: *[512:0]u8, iso_base: []const u8, inner_abs: []const u8) error{ BadPath, TooLong }!void {
    if (inner_abs.len < 2 or inner_abs[0] != '/') return error.BadPath;
    const rest = inner_abs[1..];
    if (iso_base.len + rest.len + 1 > out.len) return error.TooLong;
    @memcpy(out[0..iso_base.len], iso_base);
    @memcpy(out[iso_base.len..][0..rest.len], rest);
    out[iso_base.len + rest.len] = 0;
}

fn mountIso(dev_path: []const u8, klog_arg: ?kmsg) void {
    var dev_z: [256:0]u8 = undefined;
    if (dev_path.len >= dev_z.len) {
        if (klog_arg) |k| k.emerg("dynamod.live: device path too long", .{});
        halt();
    }
    @memcpy(dev_z[0..dev_path.len], dev_path);
    dev_z[dev_path.len] = 0;

    var target_z: [64:0]u8 = undefined;
    const iso_mp = constants.live_iso_mp;
    @memcpy(target_z[0..iso_mp.len], iso_mp);
    target_z[iso_mp.len] = 0;

    const iso9660: [*:0]const u8 = "iso9660";
    const udf: [*:0]const u8 = "udf";

    const r1 = linux.mount(
        @ptrCast(&dev_z),
        @ptrCast(&target_z),
        iso9660,
        constants.MS_RDONLY,
        0,
    );
    if (linux.E.init(r1) == .SUCCESS) {
        if (klog_arg) |k| k.info("dynamod live: mounted iso9660 on {s}", .{iso_mp});
        return;
    }

    const r2 = linux.mount(
        @ptrCast(&dev_z),
        @ptrCast(&target_z),
        udf,
        constants.MS_RDONLY,
        0,
    );
    if (linux.E.init(r2) == .SUCCESS) {
        if (klog_arg) |k| k.info("dynamod live: mounted udf on {s}", .{iso_mp});
        return;
    }

    if (klog_arg) |k| k.emerg("dynamod live: failed to mount iso9660 or udf", .{});
    halt();
}

fn attachLoopAndMountSquash(squash_path_z: [*:0]const u8, klog_arg: ?kmsg) void {
    const ctl_path: [*:0]const u8 = "/dev/loop-control";
    const ctl_fd = linux.open(ctl_path, .{ .ACCMODE = .RDWR }, 0);
    if (linux.E.init(ctl_fd) != .SUCCESS) {
        if (klog_arg) |k| k.emerg("dynamod live: cannot open /dev/loop-control", .{});
        halt();
    }
    defer _ = linux.close(@intCast(ctl_fd));

    const rc_free = linux.ioctl(@intCast(ctl_fd), LOOP_CTL_GET_FREE, 0);
    const err_free: isize = @bitCast(rc_free);
    if (err_free < 0) {
        if (klog_arg) |k| k.emerg("dynamod live: LOOP_CTL_GET_FREE failed", .{});
        halt();
    }
    const loop_num: u32 = @truncate(rc_free);

    var loop_path: [32:0]u8 = undefined;
    const lp = std.fmt.bufPrint(loop_path[0 .. loop_path.len - 1], "/dev/loop{d}", .{loop_num}) catch {
        if (klog_arg) |k| k.emerg("dynamod live: loop path fmt failed", .{});
        halt();
    };
    loop_path[lp.len] = 0;

    const loop_fd = linux.open(@ptrCast(&loop_path), .{ .ACCMODE = .RDWR }, 0);
    if (linux.E.init(loop_fd) != .SUCCESS) {
        if (klog_arg) |k| k.emerg("dynamod live: cannot open loop device", .{});
        halt();
    }
    defer _ = linux.close(@intCast(loop_fd));

    const squash_fd = linux.open(squash_path_z, .{ .ACCMODE = .RDONLY }, 0);
    if (linux.E.init(squash_fd) != .SUCCESS) {
        if (klog_arg) |k| k.emerg("dynamod live: cannot open squashfs image", .{});
        halt();
    }
    defer _ = linux.close(@intCast(squash_fd));

    const rc_set = linux.ioctl(@intCast(loop_fd), LOOP_SET_FD, @as(usize, @intCast(squash_fd)));
    const err_set: isize = @bitCast(rc_set);
    if (err_set < 0) {
        if (klog_arg) |k| k.emerg("dynamod live: LOOP_SET_FD failed", .{});
        halt();
    }

    var squash_mp_z: [64:0]u8 = undefined;
    const smp = constants.live_squash_mp;
    @memcpy(squash_mp_z[0..smp.len], smp);
    squash_mp_z[smp.len] = 0;

    const squashfs: [*:0]const u8 = "squashfs";
    const mrc = linux.mount(
        @ptrCast(&loop_path),
        @ptrCast(&squash_mp_z),
        squashfs,
        constants.MS_RDONLY,
        0,
    );
    if (linux.E.init(mrc) != .SUCCESS) {
        if (klog_arg) |k| k.emerg("dynamod live: mount squashfs failed", .{});
        halt();
    }

    if (klog_arg) |k| k.info("dynamod live: squashfs mounted on {s}", .{smp});
}

fn mountOverlayNewroot(klog_arg: ?kmsg) void {
    var opt: [512:0]u8 = undefined;
    const opt_slice = std.fmt.bufPrint(
        opt[0 .. opt.len - 1],
        "lowerdir={s},upperdir={s},workdir={s}",
        .{
            constants.live_squash_mp,
            constants.live_upper_mp,
            constants.live_work_mp,
        },
    ) catch {
        if (klog_arg) |k| k.emerg("dynamod live: overlay options too long", .{});
        halt();
    };
    opt[opt_slice.len] = 0;

    const overlay_src: [*:0]const u8 = "overlay";
    const overlay_type: [*:0]const u8 = "overlay";
    const mrc = linux.mount(
        overlay_src,
        constants.newroot_path,
        overlay_type,
        0,
        @intFromPtr(@as([*:0]const u8, @ptrCast(&opt))),
    );
    if (linux.E.init(mrc) != .SUCCESS) {
        if (klog_arg) |k| k.emerg("dynamod live: mount overlay on /newroot failed", .{});
        halt();
    }
    if (klog_arg) |k| k.info("dynamod live: overlay mounted on /newroot", .{});
}

/// Full dynamod-native ISO → squashfs → (overlay) → switch_root. Does not return.
pub fn doLiveSwitchRoot(cl: *const cmdline.Cmdline, klog_arg: ?kmsg) noreturn {
    const media = cl.getLiveMedia() orelse {
        if (klog_arg) |k| k.emerg("dynamod.live: missing dynamod.media (e.g. LABEL=... or /dev/sr0)", .{});
        halt();
    };

    if (klog_arg) |k| k.info("dynamod live: media={s}", .{media});

    switchroot.runMdev(klog_arg);

    loadLiveMediaModules(klog_arg);
    // New block devices may have appeared; refresh nodes (esp. if not only devtmpfs).
    switchroot.runMdev(klog_arg);

    const resolved = rootdev.resolve(media, cl.hasRootwait(), klog_arg) orelse {
        if (klog_arg) |k| k.emerg("dynamod.live: failed to resolve dynamod.media", .{});
        halt();
    };

    if (klog_arg) |k| k.info("dynamod live: media device {s}", .{resolved.path()});

    const delay = cl.getRootDelay();
    if (delay > 0) {
        if (klog_arg) |k| k.info("rootdelay={d}s", .{delay});
        const ts = linux.timespec{ .sec = @intCast(delay), .nsec = 0 };
        _ = linux.nanosleep(&ts, null);
    }

    mkdir_path("/run/dynamod");
    mkdir_path("/run/dynamod/live");
    mkdir_path("/run/dynamod/live/iso");
    mkdir_path("/run/dynamod/live/squash");
    mkdir_path("/run/dynamod/live/upper");
    mkdir_path("/run/dynamod/live/work");

    loadLiveFsModules(klog_arg);

    mountIso(resolved.path(), klog_arg);

    const inner = cl.getLiveSquashfsPath();
    var squash_path_buf: [512:0]u8 = undefined;
    joinIsoPath(&squash_path_buf, constants.live_iso_mp, inner) catch {
        if (klog_arg) |k| k.emerg("dynamod.squashfs must be absolute (e.g. /live/root.squashfs)", .{});
        halt();
    };

    attachLoopAndMountSquash(@ptrCast(&squash_path_buf), klog_arg);

    _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), constants.newroot_path, 0o755);

    if (cl.liveUseOverlay()) {
        mountOverlayNewroot(klog_arg);
    } else {
        var squash_mp_z: [64:0]u8 = undefined;
        const smp = constants.live_squash_mp;
        @memcpy(squash_mp_z[0..smp.len], smp);
        squash_mp_z[smp.len] = 0;
        const brc = linux.mount(
            @ptrCast(&squash_mp_z),
            constants.newroot_path,
            @ptrFromInt(0),
            MS_BIND,
            0,
        );
        if (linux.E.init(brc) != .SUCCESS) {
            if (klog_arg) |k| k.emerg("dynamod live: bind mount squash to /newroot failed", .{});
            halt();
        }
        if (klog_arg) |k| k.info("dynamod live: bind-mounted squashfs on /newroot (read-only)", .{});
    }

    switchroot.finishAfterRootMounted(klog_arg);
}
