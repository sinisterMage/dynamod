/// dynamod-init: PID 1 entry point.
///
/// This is the first userspace process started by the Linux kernel.
/// It supports two-phase boot:
///
/// 1. **Initramfs phase** (if running on ramfs/tmpfs with root= in cmdline):
///    Mount pseudo-fs, detect root device, mount it, switch_root, re-exec.
///
/// 2. **Real root phase** (normal boot or after switch_root):
///    Mount pseudo-fs, set hostname, seed entropy, spawn dynamod-svmgr.
///
/// The same binary handles both phases. Detection is automatic via statfs("/").
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
const cmdline_mod = @import("cmdline.zig");
const switchroot = @import("switchroot.zig");

pub fn main() noreturn {
    // Helper mode: create /etc/machine-id after root is remounted rw (see machine-id.toml).
    var arg_it = std.process.args();
    _ = arg_it.skip();
    if (arg_it.next()) |arg| {
        if (std.mem.eql(u8, arg, "--write-machine-id")) {
            // Do not call mountEssentialFilesystems here: it mounts tmpfs on /run (and
            // other paths). When run as a child after PID 1 already set up the namespace,
            // a successful mount stacks a fresh /run on top and hides /run/dynamod
            // (control.sock, notify sockets) from the rest of the system.
            const klog = kmsg.init();
            boot.ensureMachineId(klog);
            std.posix.exit(0);
        }
    }

    // Phase 0: Early boot
    // Mount essential pseudo-filesystems first (before we can open /dev/kmsg)
    boot.mountEssentialFilesystems(null);

    // Now we can open the kernel log
    const klog = kmsg.init();
    if (klog) |k| k.info("dynamod-init starting (PID 1)", .{});

    // Check if we're in an initramfs and need to switch_root
    if (boot.isInitramfs()) {
        const cl = cmdline_mod.Cmdline.read(klog);
        if (cl.getRoot() != null) {
            // Initramfs mode: mount real root and switch to it
            // This calls execve and does NOT return.
            switchroot.doSwitchRoot(&cl, klog);
        }
        // No root= parameter: stay in initramfs (test/development mode)
        if (klog) |k| k.info("initramfs mode: no root= param, staying in initramfs", .{});
    }

    // Phase 1: Real root boot (runs after switch_root or when booted directly)
    // Create runtime directory
    boot.createRuntimeDir(klog);

    // Set hostname
    boot.setHostname(klog);

    // Seed entropy
    boot.seedEntropy(klog);

    // /etc/machine-id is created by the machine-id oneshot after remount-root-rw
    // (writing here hits EROFS while the real root is still mounted ro).

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
