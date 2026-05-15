/// Emergency shell support.
///
/// When invoked, this opens /dev/console, fork+execs an interactive shell on
/// it, and blocks the caller (init) until the shell exits. Used when svmgr
/// is in a crash loop, when the operator sends SIGUSR2, or when
/// `dynamod.emergency=1` is set on the kernel command line.
///
/// Shell candidates are tried in order: /bin/sh, /bin/busybox sh, /sbin/sulogin.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const kmsg = @import("kmsg.zig");

/// Run an emergency shell. Blocks until the shell exits.
/// Returns true if a shell was spawned and reaped, false otherwise.
pub fn runShell(klog: ?kmsg) bool {
    if (klog) |k| k.emerg("=== entering emergency shell on /dev/console ===", .{});

    const fork_rc = linux.fork();
    const fork_err = linux.E.init(fork_rc);
    if (fork_err != .SUCCESS) {
        if (klog) |k| k.err("emergency: fork failed (errno {d})", .{@intFromEnum(fork_err)});
        return false;
    }

    const pid: linux.pid_t = @intCast(fork_rc);
    if (pid == 0) {
        // === Child ===
        // New session — required to claim /dev/console as a controlling tty.
        _ = linux.setsid();

        // Init blocks SIGCHLD/TERM/INT/USR1/USR2 via sigprocmask; restore default
        // mask in the child so the shell receives signals normally.
        var empty: linux.sigset_t = linux.sigemptyset();
        _ = linux.sigprocmask(linux.SIG.SETMASK, &empty, null);

        // Open /dev/console for read+write. We deliberately omit NOCTTY so the
        // kernel installs this as our controlling tty (we are a fresh session
        // leader with no controlling tty after setsid).
        const con_fd_rc = linux.open(constants.console_path, .{ .ACCMODE = .RDWR }, 0);
        if (linux.E.init(con_fd_rc) != .SUCCESS) {
            _ = std.posix.write(2, "dynamod-init: emergency: cannot open /dev/console\n") catch {};
            linux.exit(127);
        }
        const con_fd: i32 = @intCast(con_fd_rc);
        _ = linux.dup2(@intCast(con_fd), 0);
        _ = linux.dup2(@intCast(con_fd), 1);
        _ = linux.dup2(@intCast(con_fd), 2);
        if (con_fd > 2) _ = linux.close(con_fd);

        execShell();
    }

    // === Parent (init) ===
    if (klog) |k| k.info("emergency shell spawned (pid {d})", .{pid});

    // Block-wait for the shell to exit. We may also reap unrelated orphans
    // along the way (init is PID 1 — anything could become our child).
    while (true) {
        var status: u32 = 0;
        const rc = linux.waitpid(-1, &status, 0);
        const e = linux.E.init(rc);
        if (e == .INTR) continue;
        if (e == .CHILD) {
            if (klog) |k| k.warn("emergency: waitpid returned ECHILD before shell exit", .{});
            return false;
        }
        if (e != .SUCCESS) {
            if (klog) |k| k.err("emergency: waitpid failed (errno {d})", .{@intFromEnum(e)});
            return false;
        }
        const reaped: linux.pid_t = @intCast(rc);
        if (reaped == pid) {
            if (klog) |k| k.info("emergency shell exited (status 0x{x})", .{status});
            return true;
        }
        // Some unrelated zombie — keep waiting for the shell.
    }
}

/// Try each candidate shell in turn. execve does not return on success;
/// if every candidate fails, we exit with 127.
fn execShell() noreturn {
    const env = [_:null]?[*:0]const u8{
        "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
        "HOME=/",
        "TERM=linux",
        "PS1=(dynamod-emergency) # ",
        null,
    };

    {
        // argv[0] with leading '-' requests a login shell.
        const argv = [_:null]?[*:0]const u8{ "-sh", null };
        _ = linux.execve("/bin/sh", &argv, &env);
    }
    {
        const argv = [_:null]?[*:0]const u8{ "busybox", "sh", "-i", null };
        _ = linux.execve("/bin/busybox", &argv, &env);
    }
    {
        const argv = [_:null]?[*:0]const u8{ "sulogin", null };
        _ = linux.execve("/sbin/sulogin", &argv, &env);
    }
    _ = std.posix.write(2, "dynamod-init: emergency: no shell found (tried /bin/sh, /bin/busybox, /sbin/sulogin)\n") catch {};
    linux.exit(127);
}
