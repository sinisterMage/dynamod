# dynamod

A microarchitecture-based PID 1 init system for Linux, combining Zig's
low-level control with Rust's service management, featuring OTP-style
supervisor trees.

## Architecture

```
kernel
  └── dynamod-init (PID 1, Zig)
        ├── dynamod-svmgr (Rust) ── service management, supervisor trees
        │     ├── dynamod-logd    ── log collection
        │     └── services...
        └── (console login)
```

| Component | Language | Role |
|-----------|----------|------|
| `dynamod-init` | Zig | PID 1: mount, zombie reap, signal handling, shutdown |
| `dynamod-svmgr` | Rust | Service manager: supervisor trees, dependency DAG, cgroups |
| `dynamodctl` | Rust | CLI tool: start/stop/restart/status/list/tree/shutdown |
| `dynamod-logd` | Rust | Log daemon: collection, rotation, ring buffer |

## Features

- **OTP-style supervisor trees** — one-for-one, one-for-all, rest-for-one
  strategies with restart intensity tracking and cascading failure escalation
- **Dependency DAG** — services declare `requires`, `wants`, `after`, `before`;
  dynamic frontier algorithm maximizes startup parallelism
- **Readiness detection** — sd_notify compatible, TCP port, exec check, or immediate
- **Cgroups v2** — per-service memory, CPU, PID, and I/O limits with OOM monitoring
- **Linux namespaces** — PID, mount, network, UTS, IPC isolation per service
- **Graceful shutdown** — reverse dependency order, per-service timeouts, SIGKILL escalation
- **Crash resilience** — Zig PID 1 has no heap allocations, auto-restarts svmgr with backoff
- **MessagePack IPC** — compact binary protocol between all components

## Building

Requires Zig 0.15+ and Rust 1.75+ (2024 edition).

```sh
make          # Build all binaries
make test     # Run all unit tests
make install  # Install to /usr/local (or DESTDIR=... PREFIX=...)
```

## Quick Start

1. Create a service file:

```toml
# /etc/dynamod/services/myapp.toml
[service]
name = "myapp"
exec = ["/usr/bin/myapp", "--config", "/etc/myapp.conf"]

[restart]
policy = "permanent"

[readiness]
type = "tcp-port"
port = 8080
```

2. Control services:

```sh
dynamodctl list              # List all services
dynamodctl start myapp       # Start a service
dynamodctl status myapp      # Check status
dynamodctl tree              # View supervisor tree
dynamodctl shutdown reboot   # Reboot the system
```

## Configuration

- Services: `/etc/dynamod/services/*.toml`
- Supervisors: `/etc/dynamod/supervisors/*.toml`

See [docs/configuration.md](docs/configuration.md) for the full reference.

## Documentation

- [Architecture](docs/architecture.md) — system design, IPC protocol, boot/shutdown sequences
- [Configuration](docs/configuration.md) — TOML schema reference with all fields

## Testing

```sh
make test          # Unit tests (Zig + Rust)
make test-qemu     # Boot in QEMU with dynamod as PID 1
```

## Project Structure

```
dynamod/
├── zig/src/           # PID 1 init (Zig)
│   ├── main.zig       # Entry point
│   ├── boot.zig       # Early boot (mount, hostname, entropy)
│   ├── event_loop.zig # epoll main loop
│   ├── signal.zig     # signalfd handler
│   ├── reaper.zig     # Zombie reaping
│   ├── child.zig      # Spawn/monitor svmgr
│   ├── shutdown.zig   # Shutdown sequence
│   ├── ipc.zig        # IPC framing
│   ├── msgpack.zig    # MessagePack codec
│   ├── kmsg.zig       # Kernel log writer
│   └── constants.zig  # Well-known paths
├── rust/
│   ├── dynamod-svmgr/ # Service manager
│   │   └── src/
│   │       ├── supervisor/  # OTP supervisor trees
│   │       ├── dependency/  # DAG + frontier algorithm
│   │       ├── process/     # Spawn, monitor, readiness
│   │       ├── cgroup/      # Cgroups v2 isolation
│   │       ├── namespace/   # Linux namespaces
│   │       ├── ipc/         # Init channel + control socket
│   │       ├── config/      # TOML parsing + validation
│   │       └── shutdown.rs  # Graceful shutdown
│   ├── dynamodctl/    # CLI tool
│   ├── dynamod-logd/  # Log daemon
│   └── dynamod-common/# Shared protocol + types
├── config/            # Example service configs
├── docs/              # Documentation
└── test/qemu/         # QEMU integration tests
```

## License

MIT
