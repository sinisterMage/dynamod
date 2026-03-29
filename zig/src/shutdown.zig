/// Shutdown sequence for dynamod-init.
/// Handles graceful shutdown: signal processes, unmount filesystems, reboot/halt.
///
/// Sequence:
/// 1. Send SIGTERM to all processes (except PID 1)
/// 2. Wait shutdown_sigterm_timeout_s, reaping zombies
/// 3. Send SIGKILL to all remaining processes
/// 4. Wait shutdown_sigkill_timeout_s, reaping zombies
/// 5. Save entropy to random seed file
/// 6. Sync all filesystems
/// 7. Unmount all filesystems in reverse order
/// 8. Remount / read-only
/// 9. Final sync
/// 10. Call reboot(2) with the appropriate command
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");
const reaper = @import("reaper.zig");

pub const ShutdownKind = enum {
    poweroff,
    reboot,
    halt,
};

/// Execute the full shutdown sequence.
/// This function does not return — it ends with a reboot(2) syscall.
pub fn execute(kind: ShutdownKind, klog: ?kmsg) noreturn {
    if (klog) |k| k.emerg("initiating {s}", .{@tagName(kind)});

    // Step 1: Send SIGTERM to all processes (except ourselves — PID 1)
    if (klog) |k| k.info("sending SIGTERM to all processes", .{});
    _ = linux.kill(-1, linux.SIG.TERM);

    // Step 2: Wait for processes to exit, actively reaping
    if (klog) |k| k.info("waiting {d}s for processes to terminate", .{constants.shutdown_sigterm_timeout_s});
    waitAndReap(constants.shutdown_sigterm_timeout_s, klog);

    // Step 3: Send SIGKILL to remaining processes
    if (klog) |k| k.info("sending SIGKILL to remaining processes", .{});
    _ = linux.kill(-1, linux.SIG.KILL);

    // Step 4: Wait briefly and reap
    waitAndReap(constants.shutdown_sigkill_timeout_s, klog);

    // Step 5: Save random seed for next boot
    saveRandomSeed(klog);

    // Step 6: Sync filesystems
    if (klog) |k| k.info("syncing filesystems", .{});
    linux.sync();

    // Step 7: Unmount all pseudo-filesystems (best effort, reverse order)
    unmountAll(klog);

    // Step 8: Remount / read-only
    remountRootReadonly(klog);

    // Step 9: Final sync
    linux.sync();

    // Step 10: Final reboot syscall
    if (klog) |k| k.emerg("{s} now", .{@tagName(kind)});

    const cmd: linux.LINUX_REBOOT.CMD = switch (kind) {
        .poweroff => .POWER_OFF,
        .reboot => .RESTART,
        .halt => .HALT,
    };

    _ = linux.reboot(.MAGIC1, .MAGIC2, cmd, null);

    // Should never reach here
    while (true) {
        _ = linux.pause();
    }
}

/// Wait for a given number of seconds, actively reaping zombies every 100ms.
fn waitAndReap(secs: u32, klog: ?kmsg) void {
    const iterations = secs * 10; // 100ms per iteration
    const sleep_ts = linux.timespec{ .sec = 0, .nsec = 100_000_000 };
    var i: u32 = 0;
    while (i < iterations) : (i += 1) {
        const reaped = reaper.reapAll(klog, null);
        _ = reaped;
        _ = linux.nanosleep(&sleep_ts, null);
    }
}

fn saveRandomSeed(klog: ?kmsg) void {
    const urandom = std.fs.openFileAbsolute("/dev/urandom", .{}) catch return;
    defer urandom.close();

    var buf: [512]u8 = undefined;
    const len = urandom.read(&buf) catch return;
    if (len == 0) return;

    // Ensure parent directory exists
    _ = linux.mkdirat(@bitCast(@as(i32, linux.AT.FDCWD)), "/var/lib/dynamod", 0o755);

    const seed_file = std.fs.createFileAbsolute(constants.random_seed_path, .{}) catch return;
    defer seed_file.close();
    _ = seed_file.write(buf[0..len]) catch return;

    if (klog) |k| k.info("saved random seed ({d} bytes)", .{len});
}

fn unmountAll(klog: ?kmsg) void {
    // Unmount in reverse order of mounting
    const targets = [_][*:0]const u8{
        "/sys/fs/cgroup",
        "/dev/shm",
        "/dev/pts",
        "/run",
        "/dev",
        "/sys",
        "/proc",
    };

    for (&targets) |target| {
        const name = std.mem.span(target);
        const rc = linux.umount2(target, linux.MNT.DETACH);
        const e = linux.E.init(rc);
        if (e != .SUCCESS and e != .INVAL and e != .NOENT) {
            if (klog) |k| k.warn("failed to unmount {s}: errno {d}", .{ name, @intFromEnum(e) });
        } else if (e == .SUCCESS) {
            if (klog) |k| k.info("unmounted {s}", .{name});
        }
    }
}

fn remountRootReadonly(klog: ?kmsg) void {
    const MS_REMOUNT = 0x20;
    const MS_RDONLY = 0x01;
    const rc = linux.mount("", "/", "", MS_REMOUNT | MS_RDONLY, 0);
    const e = linux.E.init(rc);
    if (e != .SUCCESS) {
        if (klog) |k| k.warn("failed to remount / read-only: errno {d}", .{@intFromEnum(e)});
    } else {
        if (klog) |k| k.info("remounted / read-only", .{});
    }
}
