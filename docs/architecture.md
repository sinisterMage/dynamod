# Architecture

dynamod is a Linux-only PID 1 init system with a microkernel-inspired design.
Each component runs as its own process, communicating over Unix domain sockets
with MessagePack-framed messages. The init itself is intentionally tiny — all
the interesting logic lives in the service manager.

## The big picture

```
kernel
  └── dynamod-init (PID 1, Zig)
        │
        │  Phase A (initramfs):
        │    mount /proc, /sys, /dev
        │    parse root= from /proc/cmdline
        │    mount root device on /newroot
        │    switch_root → re-exec from real rootfs
        │
        │  Phase B (real root):
        │    mount remaining pseudo-fs
        │    set hostname, seed entropy
        │    spawn dynamod-svmgr via socketpair
        │    enter epoll event loop
        │
        ├── dynamod-svmgr (Rust)
        │     ├── [supervisor: early-boot] (one-for-all)
        │     │     ├── fsck
        │     │     ├── remount-root-rw
        │     │     ├── fstab-mount
        │     │     ├── modules-load
        │     │     ├── mdev-coldplug
        │     │     ├── bootmisc
        │     │     └── network
        │     │
        │     ├── [supervisor: desktop] (one-for-one)
        │     │     ├── dbus
        │     │     ├── dynamod-logind    (login1)
        │     │     ├── dynamod-sd1bridge (systemd1)
        │     │     └── dynamod-hostnamed (hostname1)
        │     │
        │     ├── sshd
        │     ├── nginx
        │     ├── postgresql
        │     └── ...
        │
        └── (console login on tty1)
```

## Components

### dynamod-init (Zig)

The PID 1 process. Intentionally minimal (~2300 lines of Zig across 14 files)
to minimize the chance of a crash that would bring down the system.

**What it does:**

- **Two-phase boot**: detects whether it's in an initramfs (via `statfs("/")`),
  and if `root=` is on the kernel cmdline, performs the full initramfs-to-rootfs
  transition (mount root, move pseudo-fs, delete initramfs, `switch_root`, re-exec)
- **Mount pseudo-filesystems**: `/proc`, `/sys`, `/dev`, `/dev/pts`, `/dev/shm`,
  `/run`, `/sys/fs/cgroup`
- **Early system setup**: set hostname, seed entropy, generate `/etc/machine-id`
- **Spawn and babysit dynamod-svmgr**: restart with exponential backoff on crash
  (500ms initial, 30s max)
- **Zombie reaping**: collects exit status for all orphaned processes
- **Signal handling**: via `signalfd` + `epoll` (SIGCHLD, SIGTERM, SIGINT, SIGUSR1/2)
- **Shutdown**: SIGTERM all → wait 5s → SIGKILL → sync → unmount all → remount
  root read-only → `reboot(2)`

**Design constraints:**

- No heap allocations after initialization
- No panics — every error path is handled
- Fixed-size buffers only (4KB cmdline, 8KB dirent, etc.)
- Statically linked against musl (no libc dependency at runtime)

### dynamod-svmgr (Rust)

The service manager — the "brain" of the system.

**What it does:**

- Parses TOML service and supervisor configurations from `/etc/dynamod/`
- Builds and validates the dependency DAG (rejects cycles)
- Manages OTP-style supervisor trees (see below)
- Spawns services via `fork`/`exec` with namespace and cgroup isolation
- Runs a dynamic frontier algorithm for maximum-parallelism startup
- Detects readiness via sd_notify, TCP port, exec check, or file descriptor
- Handles graceful shutdown in reverse dependency order
- Listens on `/run/dynamod/control.sock` for `dynamodctl` commands
- Maintains a heartbeat protocol with dynamod-init (5s interval)

### dynamodctl (Rust)

CLI tool for operators. Connects to the control socket.

```
dynamodctl list              # List all services with status
dynamodctl start <name>      # Start a service
dynamodctl stop <name>       # Stop a service
dynamodctl restart <name>    # Restart a service
dynamodctl status <name>     # Show service details
dynamodctl tree              # Display the supervisor tree
dynamodctl shutdown <mode>   # poweroff, reboot, or halt
```

### dynamod-logd (Rust)

Log collection daemon. Accepts log streams from services, stores them in
rotating log files under `/var/log/dynamod/`, and maintains an in-memory
ring buffer for fast queries.

### systemd-mimic layer

Four components that implement systemd's frontend D-Bus APIs so that desktop
environments, Wayland compositors, and common Linux tools work without
modification. All are clean-room implementations based on freedesktop.org
specs — no systemd source code, MIT licensed.

| Component | Bus Name | Key Features |
|-----------|----------|-------------|
| **dynamod-logind** | `org.freedesktop.login1` | Session/seat/user management, `TakeControl`/`TakeDevice` for Wayland GPU access, power management, inhibitor locks, VT switching |
| **dynamod-sd1bridge** | `org.freedesktop.systemd1` | Translates `StartUnit`/`StopUnit`/`ListUnits` to dynamod's native IPC; makes `systemctl` work |
| **dynamod-hostnamed** | `hostname1`, `timedate1`, `locale1` | System identification for GNOME Settings panels |
| **dynamod-sdnotify** | — | Drop-in `libsystemd.so.0` providing `sd_notify()`, `sd_listen_fds()`, `sd_booted()`, `sd_journal_print()`, `sd_pid_get_session()` |

Verified working with **sway** (Wayland compositor via seatd) and
**GNOME Shell 47** (Mutter acquiring GPU via logind's TakeDevice).

## Boot sequence

### Phase A: Initramfs (when `root=` is on the cmdline)

1. Kernel unpacks initramfs, executes `dynamod-init` as PID 1
2. Mount `/proc`, `/sys`, `/dev` (3 pseudo-filesystems)
3. Read `/proc/cmdline`, extract `root=`, `rootfstype=`, `rootflags=`, `rootwait`
4. Run `mdev -s` to create device nodes (needed for UUID/LABEL resolution)
5. Resolve root device:
   - `/dev/sda1` → use directly
   - `UUID=xxxx` → scan `/dev/disk/by-uuid/`
   - `PARTUUID=xxxx` → scan `/dev/disk/by-partuuid/`
   - `LABEL=xxxx` → scan `/dev/disk/by-label/`
6. Wait for device if `rootwait` is set (poll every 250ms, max 30s)
7. Mount root device on `/newroot` (read-only by default)
8. Move `/proc`, `/sys`, `/dev` to `/newroot/proc`, `/newroot/sys`, `/newroot/dev`
9. Delete initramfs contents (recursive, skip mount points) to free RAM
10. `chdir("/newroot")` → `mount(".", "/", MS_MOVE)` → `chroot(".")` → `chdir("/")`
11. `execve("/sbin/dynamod-init")` — re-exec from the real rootfs

### Phase B: Real root

1. Mount remaining pseudo-filesystems: `/dev/pts`, `/dev/shm`, `/run`, `/sys/fs/cgroup`
   (already-mounted paths from Phase A return EBUSY, which is silently ignored)
2. Create `/run/dynamod/` runtime directory
3. Set hostname from `/etc/hostname`
4. Seed kernel entropy from `/var/lib/dynamod/random-seed`
5. Generate `/etc/machine-id` if missing (32 hex chars from `/dev/urandom`)
6. Set up signal handling via `signalfd`
7. Create socketpair, `fork`/`exec` dynamod-svmgr, pass one end via `DYNAMOD_INIT_FD`
8. Enter `epoll` event loop: signals, svmgr pidfd, IPC socket

### Phase C: Service startup (in dynamod-svmgr)

1. Load all `*.toml` from `/etc/dynamod/services/` and `/etc/dynamod/supervisors/`
2. Validate configs (no cycles, cross-references check)
3. Initialize cgroup v2 hierarchy at `/sys/fs/cgroup/dynamod/`
4. Bind control socket at `/run/dynamod/control.sock`
5. Build dependency graph, compute initial frontier
6. Start all services with zero unmet dependencies in parallel
7. As services become ready, decrement unmet counts for dependents
8. If a service fails, block all transitive `requires` dependents
9. Log startup summary: "N ready, M blocked"

Typical boot on a disk-based system:

```
fsck → remount-root-rw → fstab-mount → (bootmisc, hostname, sysctl, network, modules-load)
  → syslog → dynamod-logd → mdev-coldplug → (getty, dbus → logind → sd1bridge → hostnamed)
```

## Shutdown sequence

1. Svmgr computes reverse topological order (dependents stop first)
2. For each service: run `stop-exec` if configured, else send `stop-signal`
   (default SIGTERM), wait `stop-timeout` (default 10s), SIGKILL if still alive
3. Clean up cgroup for each stopped service
4. Svmgr sends `RequestShutdown` to init, then exits
5. Init sends SIGTERM to all remaining processes, waits 5s reaping zombies
6. Init sends SIGKILL to remaining processes, waits 2s
7. Save random seed to `/var/lib/dynamod/random-seed`
8. `sync()` all filesystems
9. Read `/proc/mounts`, unmount everything in reverse order (falls back to
   hardcoded list if `/proc/mounts` is unreadable)
10. Remount `/` read-only
11. Final `sync()`, then `reboot(2)` syscall (poweroff/reboot/halt)

## IPC protocol

All inter-component communication uses length-prefixed MessagePack over
Unix domain sockets (SOCK_STREAM).

```
┌──────────┬──────────────┬─────────────────────┐
│ magic    │ length       │ payload              │
│ 0x44 0x4D│ u32 LE (4B) │ MessagePack (N bytes)│
│ "DM"     │              │                      │
└──────────┴──────────────┴─────────────────────┘
```

- **Magic**: `0x44 0x4D` ("DM") for quick validation and resync
- **Max payload**: 64 KiB
- **Serialization**: `rmp-serde` with `struct_as_map()` on the Rust side
  (string keys), hand-written minimal decoder on the Zig side (~300 lines)

### Channels

| Channel | Transport | Messages |
|---------|-----------|----------|
| init ↔ svmgr | socketpair (pre-fork) | Heartbeat, HeartbeatAck, RequestShutdown, ShutdownSignal, LogToKmsg |
| svmgr ↔ dynamodctl | `/run/dynamod/control.sock` | StartService, StopService, RestartService, ServiceStatus, ListServices, TreeStatus, Reload, Shutdown, GetServiceByPid |

## Supervisor trees

Inspired by Erlang/OTP. Services are organized into a tree of supervisors,
each with its own restart strategy and intensity limits.

### Restart strategies

- **one-for-one**: Only the failed child is restarted. Good for independent services.
- **one-for-all**: All children stop (reverse order) then restart (start order).
  Use when children are tightly coupled.
- **rest-for-one**: The failed child and all children started after it are restarted.
  Use when later children depend on earlier ones.

### Restart intensity

Each supervisor tracks restart timestamps in a sliding window. If
`max_restarts` is exceeded within `max_restart_window`, the supervisor
itself fails and the failure escalates to its parent.

For example, with `max-restarts = 5` and `max-restart-window = "60s"`:
if a child crashes 6 times in one minute, the supervisor gives up and
escalates to its parent.

### Per-service restart policies

- **permanent**: Always restart, no matter how it exited
- **transient**: Restart only on abnormal exit (non-zero code or signal)
- **temporary**: Never restart (for oneshot tasks)

## Dependency resolution

Services declare ordering and hard/soft dependencies:

- `requires = ["network"]` — hard dependency: if network fails, this service is blocked
- `wants = ["syslog"]` — soft dependency: ordering hint, but won't block if absent
- `after = ["bootmisc"]` — pure ordering: start after bootmisc, no failure propagation
- `before = ["nginx"]` — reverse ordering: ensure this starts before nginx

### Dynamic frontier algorithm

Instead of computing a single topological order upfront, dynamod maintains
a live frontier of services whose dependencies are all satisfied:

1. Compute `unmet[s]` = count of unready dependencies for each service
2. Start all services with `unmet == 0` in parallel
3. When a service becomes READY: decrement `unmet` for all its dependents;
   if any reach zero, add them to the frontier
4. When a service FAILS: block all transitive `requires` dependents;
   `after`-only dependents can still start

This maximizes parallelism. In a diamond dependency (A → B, A → C, B+C → D),
B and C start simultaneously after A is ready.

Cycles are detected via DFS with three-color marking before startup.
If found, dynamod logs the exact cycle path and refuses to start.

## Resource isolation

### Cgroups v2

Each service gets its own cgroup at `/sys/fs/cgroup/dynamod/<service>/`.
Configurable limits:

- `memory.max` / `memory.high` — hard OOM limit and soft reclaim pressure
- `cpu.weight` / `cpu.max` — scheduling weight and bandwidth
- `pids.max` — process count limit
- `io.weight` — I/O scheduling weight

OOM kills are detected by polling `memory.events`.

### Linux namespaces

Per-service isolation via `unshare(2)`:

- **pid**: isolated PID namespace (service sees itself as PID 1)
- **mnt**: private mount namespace with optional read-only root, private `/tmp`, bind mounts
- **net**: network namespace isolation
- **uts**: separate hostname
- **ipc**: isolated System V IPC
- **user**: user namespace mapping
- **cgroup**: isolated cgroup view
