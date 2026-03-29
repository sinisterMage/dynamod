/// dynamod-init: PID 1 entry point.
///
/// This is the first userspace process started by the Linux kernel.
/// It performs early boot (mount pseudo-filesystems, set hostname),
/// spawns the service manager (dynamod-svmgr), and enters the main
/// event loop for signal handling and zombie reaping.
///
/// Design constraints:
/// - No heap allocations after initialization
/// - All error paths handled (must never crash)
/// - Fixed-size buffers only
const std = @import("std");
const linux = std.os.linux;

const boot = @import("boot.zig");
const kmsg = @import("kmsg.zig");
const signal = @import("signal.zig");
const reaper = @import("reaper.zig");
const child = @import("child.zig");
const event_loop = @import("event_loop.zig");
const constants = @import("constants.zig");
const shutdown_mod = @import("shutdown.zig");

pub fn main() noreturn {
    // Phase 0: Early boot
    // Mount essential pseudo-filesystems first (before we can open /dev/kmsg)
    boot.mountEssentialFilesystems(null);

    // Now we can open the kernel log
    const klog = kmsg.init();
    if (klog) |k| k.info("dynamod-init starting (PID 1)", .{});

    // Create runtime directory
    boot.createRuntimeDir(klog);

    // Set hostname
    boot.setHostname(klog);

    // Seed entropy
    boot.seedEntropy(klog);

    // Generate /etc/machine-id if missing (needed for D-Bus and desktop tools)
    boot.ensureMachineId(klog);

    if (klog) |k| k.info("early boot complete", .{});

    // Phase 1: Set up signal handling
    const sig = signal.init() catch {
        if (klog) |k| k.emerg("failed to set up signal handling", .{});
        shutdown_mod.execute(.halt, klog);
    };

    // Phase 2: Spawn the service manager
    var svmgr = child.create(constants.svmgr_path);
    svmgr.spawn(klog) catch {
        if (klog) |k| k.emerg("failed to spawn dynamod-svmgr", .{});
        shutdown_mod.execute(.halt, klog);
    };

    if (klog) |k| k.info("service manager launched, entering event loop", .{});

    // Phase 3: Enter main event loop
    var loop = event_loop.init(sig, &svmgr, klog) catch {
        if (klog) |k| k.emerg("failed to initialize event loop", .{});
        shutdown_mod.execute(.halt, klog);
    };

    loop.registerSvmgr() catch {
        if (klog) |k| k.warn("failed to register svmgr with epoll", .{});
    };

    loop.run();
}
