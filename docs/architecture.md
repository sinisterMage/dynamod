# Dynamod Architecture

Dynamod is a Linux-only PID 1 init system with a microkernel-inspired architecture.
Each component runs as its own process, communicating via Unix domain sockets
with MessagePack-framed messages.

## Components

```
kernel
  └── dynamod-init (PID 1, Zig)
        ├── dynamod-svmgr (Rust)
        │     ├── dynamod-logd (Rust)
        │     ├── [supervisor: network]
        │     │     ├── dhcpcd
        │     │     └── resolved
        │     └── [supervisor: data]
        │           └── postgresql
        └── (agetty on tty1)
```

### dynamod-init (Zig)

The PID 1 process. Intentionally minimal (~600 lines of Zig) to minimize
the chance of a crash that would bring down the entire system.

**Responsibilities:**
- Mount essential pseudo-filesystems (`/proc`, `/sys`, `/dev`, `/run`, `/sys/fs/cgroup`)
- Set hostname, seed entropy
- Spawn and monitor `dynamod-svmgr` (restart with exponential backoff on crash)
- Zombie reaping for all orphaned processes
- Signal handling via `signalfd` + `epoll`
- System shutdown sequence (SIGTERM → SIGKILL → sync → unmount → reboot)
- Forward shutdown requests between kernel signals and svmgr

**Design constraints:**
- No heap allocations after initialization
- No panics — all error paths handled
- Fixed-size buffers only
- Statically linked (no libc dependency)

### dynamod-svmgr (Rust)

The service manager. The "brain" of the system.

**Responsibilities:**
- Parse TOML service and supervisor configurations
- Build and validate the dependency DAG (cycle detection)
- Manage OTP-style supervisor trees (one-for-one, one-for-all, rest-for-one)
- Spawn services via fork/exec with namespace and cgroup isolation
- Dynamic frontier algorithm for maximum-parallelism startup
- Readiness detection (sd_notify, TCP port, exec check, immediate)
- Graceful shutdown in reverse dependency order
- Control socket for `dynamodctl`
- Heartbeat protocol with `dynamod-init`

### dynamodctl (Rust)

CLI tool for operators. Connects to the control socket at
`/run/dynamod/control.sock`.

**Commands:**
- `dynamodctl start <name>` — Start a service
- `dynamodctl stop <name>` — Stop a service
- `dynamodctl restart <name>` — Restart a service
- `dynamodctl status <name>` — Show service status
- `dynamodctl list` — List all services with status
- `dynamodctl tree` — Show the supervisor tree
- `dynamodctl shutdown [poweroff|reboot|halt]` — System shutdown

### dynamod-logd (Rust)

Log collection daemon. Accepts log streams from services, stores them
in rotating log files under `/var/log/dynamod/`, and maintains an
in-memory ring buffer for fast queries.

## IPC Protocol

All inter-component communication uses length-prefixed MessagePack over
Unix domain sockets (SOCK_STREAM).

```
[magic: 0x444D (2B)] [length: u32 LE (4B)] [payload: MessagePack (N B)]
```

- **Magic:** `0x44 0x4D` ("DM") for quick validation
- **Max message:** 64 KiB
- **Serialization:** MessagePack via `rmp-serde` (Rust) and a minimal
  hand-written decoder (Zig, ~270 lines)

### Channels

| Channel | Transport | Purpose |
|---------|-----------|---------|
| init ↔ svmgr | socketpair (created before fork) | Heartbeat, shutdown requests |
| svmgr ↔ dynamodctl | `/run/dynamod/control.sock` | Start/stop/status/tree commands |

## Supervisor Trees

Dynamod implements OTP-style supervisor trees from Erlang/OTP.

### Restart Strategies

- **one-for-one:** Only the failed child is restarted
- **one-for-all:** All children are stopped (reverse order) and restarted (start order)
- **rest-for-one:** The failed child and all children started after it are restarted

### Restart Intensity

Each supervisor tracks restarts in a time window (ring buffer of timestamps).
If `max_restarts` is exceeded within `max_restart_window`, the supervisor
itself fails and the failure escalates to its parent supervisor.

### Restart Policies

Per-service:
- **permanent:** Always restart, regardless of exit reason
- **transient:** Restart only on abnormal exit (non-zero code or signal)
- **temporary:** Never restart

## Dependency Resolution

Services declare dependencies via `requires`, `wants`, `after`, and `before`.

### Dynamic Frontier Algorithm

Instead of computing a single topological order, dynamod maintains a
frontier of services whose dependencies are all satisfied:

1. Compute `unmet[s]` = count of unready dependencies for each service
2. Start all services with `unmet == 0` in parallel
3. When a service becomes READY, decrement `unmet` for all its dependents
4. When a service FAILS: block all transitive `requires` dependents;
   `after`-only dependents can still start

This maximizes startup parallelism — e.g., in a diamond dependency
(A → B, A → C, B+C → D), B and C start simultaneously after A is ready.

### Cycle Detection

Uses DFS with three-color marking. If cycles are detected at boot,
dynamod refuses to start and logs the exact cycle path.

## Resource Isolation

### Cgroups v2

Each service gets its own cgroup under `/sys/fs/cgroup/dynamod/<service>/`.
Configurable limits:
- `memory.max` / `memory.high` — hard and soft memory limits
- `cpu.weight` / `cpu.max` — CPU scheduling weight and bandwidth
- `pids.max` — maximum number of processes
- `io.weight` — I/O scheduling weight

OOM kills are detected by polling `memory.events`.

### Linux Namespaces

Per-service namespace isolation configured in TOML:
- **pid:** Isolated PID namespace (service sees itself as PID 1)
- **mnt:** Private mount namespace with optional read-only root, private `/tmp`, bind mounts
- **net:** Network namespace isolation
- **uts/ipc/user/cgroup:** Additional namespace types

## Boot Sequence

1. Kernel executes `dynamod-init` as PID 1
2. Init mounts `/proc`, `/sys`, `/dev`, `/run`, `/sys/fs/cgroup`
3. Init creates socketpair, fork/execs `dynamod-svmgr`
4. Init enters epoll loop (signalfd + pidfd + IPC socket)
5. Svmgr loads config, validates dependency DAG (no cycles)
6. Svmgr initializes cgroup hierarchy
7. Svmgr runs dynamic frontier: starts services as deps become ready
8. Services signal readiness; dependents start when all deps satisfied

## Shutdown Sequence

1. Svmgr computes reverse topological order (dependents first)
2. For each service: send configured stop-signal, wait stop-timeout, SIGKILL if needed
3. Clean up cgroups for each stopped service
4. Svmgr sends `RequestShutdown` to init, then exits
5. Init sends SIGTERM to all remaining processes, waits 5s, SIGKILL
6. Init saves random seed, syncs filesystems
7. Init unmounts all pseudo-filesystems, remounts `/` read-only
8. Init calls `reboot(2)` (poweroff/reboot/halt)
