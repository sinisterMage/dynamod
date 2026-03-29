/// Zombie reaping for PID 1.
/// As PID 1, we are responsible for reaping ALL orphaned child processes,
/// not just our direct children. This is a fundamental PID 1 responsibility.
const std = @import("std");
const linux = std.os.linux;
const kmsg = @import("kmsg.zig");

/// Information about a reaped child process.
pub const ReapedChild = struct {
    pid: linux.pid_t,
    status: u32,

    pub fn exitedNormally(self: ReapedChild) bool {
        return linux.W.IFEXITED(self.status);
    }

    pub fn exitCode(self: ReapedChild) ?u8 {
        if (linux.W.IFEXITED(self.status)) {
            return linux.W.EXITSTATUS(self.status);
        }
        return null;
    }

    pub fn termSignal(self: ReapedChild) ?u32 {
        if (linux.W.IFSIGNALED(self.status)) {
            return linux.W.TERMSIG(self.status);
        }
        return null;
    }
};

/// Reap all available zombie children (non-blocking).
/// Calls the callback for each reaped child.
/// Returns the number of children reaped.
pub fn reapAll(klog: ?kmsg, callback: ?*const fn (ReapedChild) void) u32 {
    var count: u32 = 0;
    while (true) {
        var status: u32 = 0;
        const rc = linux.waitpid(-1, &status, linux.W.NOHANG);
        const e = linux.E.init(rc);

        if (e == .CHILD) break; // No more children
        if (e != .SUCCESS) break; // Other error

        const pid: linux.pid_t = @intCast(rc);
        if (pid == 0) break; // No more zombies

        count += 1;
        const child = ReapedChild{ .pid = pid, .status = status };

        if (klog) |k| {
            if (child.exitCode()) |code| {
                k.info("reaped pid {d} (exit code {d})", .{ pid, code });
            } else if (child.termSignal()) |sig| {
                k.info("reaped pid {d} (signal {d})", .{ pid, sig });
            } else {
                k.info("reaped pid {d} (status 0x{x})", .{ pid, status });
            }
        }

        if (callback) |cb| {
            cb(child);
        }
    }
    return count;
}
