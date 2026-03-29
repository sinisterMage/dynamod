/// Signal handling via signalfd for dynamod-init.
/// We block signals and read them via a file descriptor so they can be
/// integrated into the epoll event loop without races.
const std = @import("std");
const linux = std.os.linux;

const Self = @This();

fd: std.posix.fd_t,

/// Block SIGCHLD, SIGTERM, SIGINT, SIGUSR1, SIGUSR2 and create a signalfd.
pub fn init() !Self {
    var mask = linux.sigemptyset();
    linux.sigaddset(&mask, linux.SIG.CHLD);
    linux.sigaddset(&mask, linux.SIG.TERM);
    linux.sigaddset(&mask, linux.SIG.INT);
    linux.sigaddset(&mask, linux.SIG.USR1);
    linux.sigaddset(&mask, linux.SIG.USR2);

    // Block these signals so they're delivered via signalfd
    const rc = linux.sigprocmask(linux.SIG.BLOCK, &mask, null);
    const sigproc_err = linux.E.init(rc);
    if (sigproc_err != .SUCCESS) {
        return error.SigprocmaskFailed;
    }

    const sfd = linux.signalfd(-1, &mask, linux.SFD.NONBLOCK | linux.SFD.CLOEXEC);
    const sfd_err = linux.E.init(sfd);
    if (sfd_err != .SUCCESS) {
        return error.SignalfdFailed;
    }

    return Self{ .fd = @intCast(sfd) };
}

/// Signal types we handle.
pub const SignalEvent = enum {
    child_exited,
    shutdown_term,
    shutdown_int,
    user1,
    user2,
    unknown,
};

/// Read one signal from the signalfd. Returns null if no signal is pending.
pub fn read(self: Self) ?SignalEvent {
    var info: linux.signalfd_siginfo = undefined;
    const bytes = std.posix.read(self.fd, std.mem.asBytes(&info)) catch return null;
    if (bytes != @sizeOf(linux.signalfd_siginfo)) return null;

    return switch (info.signo) {
        linux.SIG.CHLD => .child_exited,
        linux.SIG.TERM => .shutdown_term,
        linux.SIG.INT => .shutdown_int,
        linux.SIG.USR1 => .user1,
        linux.SIG.USR2 => .user2,
        else => .unknown,
    };
}
