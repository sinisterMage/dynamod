# dynamod

A Linux init system built from scratch in Zig and Rust. It combines a minimal,
crash-proof PID 1 with a powerful service manager inspired by Erlang/OTP's
supervisor trees.

dynamod boots your system, manages services with dependency-aware parallel startup,
isolates them with cgroups and namespaces, and provides systemd-compatible D-Bus
interfaces so desktop environments like GNOME and Wayland compositors like sway
work out of the box.

## How it works

```
kernel
  └── dynamod-init (PID 1, Zig)
        │
        ├── [initramfs mode] detect root device, mount ext4/btrfs, switch_root
        │
        ├── dynamod-svmgr (Rust) ── service management
        │     ├── dynamod-logd       ── log collection
        │     ├── dynamod-logind     ── session/seat management (login1)
        │     ├── dynamod-sd1bridge  ── systemctl compatibility (systemd1)
        │     ├── dynamod-hostnamed  ── hostname/timezone/locale (hostname1)
        │     ├── dbus-daemon        ── D-Bus system bus
        │     ├── sway / gnome-shell ── Wayland desktop
        │     └── your services...
        │
        └── (console login)
```

dynamod has three layers:

| Component | Language | What it does |
|-----------|----------|-------------|
| **dynamod-init** | Zig | PID 1. Mounts filesystems, handles initramfs-to-rootfs transition, reaps zombies, manages shutdown. ~2300 lines across 14 files, no heap allocations, can't crash. |
| **dynamod-svmgr** | Rust | The service manager. Reads TOML configs, builds a dependency graph, starts services in parallel, supervises them with OTP-style restart strategies, and enforces resource limits via cgroups v2. |
| **dynamodctl** | Rust | CLI tool. `dynamodctl start nginx`, `dynamodctl tree`, `dynamodctl shutdown reboot`. |

Plus the **systemd-mimic** layer for desktop compatibility:

| Component | D-Bus Interface | Purpose |
|-----------|----------------|---------|
| **dynamod-logind** | `org.freedesktop.login1` | Session/seat management, `TakeDevice` for Wayland GPU access |
| **dynamod-sd1bridge** | `org.freedesktop.systemd1` | Makes `systemctl` and GNOME service management work |
| **dynamod-hostnamed** | `hostname1`, `timedate1`, `locale1` | GNOME Settings panels |
| **dynamod-sdnotify** | — | Drop-in `libsystemd.so` shim (`sd_notify`, `sd_listen_fds`, etc.) |

## Features

- **Two-phase boot** — boots from initramfs, detects root device from kernel
  cmdline (`root=UUID=...`), mounts it, and `switch_root`s to the real filesystem
- **OTP-style supervisor trees** — one-for-one, one-for-all, rest-for-one
  strategies with restart intensity tracking and cascading failure escalation
- **Dependency-aware parallel startup** — services declare `requires`, `wants`,
  `after`, `before`; a dynamic frontier algorithm starts everything as fast as possible
- **Readiness detection** — sd_notify, TCP port polling, exec health checks, or
  file descriptor signaling
- **Cgroups v2** — per-service memory, CPU, PID, and I/O limits with OOM detection
- **Linux namespaces** — PID, mount, network, UTS, IPC isolation per service
- **systemd compatibility** — D-Bus interfaces for login1, systemd1, hostname1;
  Wayland compositors and GNOME Shell work without modification
- **Graceful shutdown** — reverse dependency order, per-service stop commands
  and timeouts, SIGKILL escalation, filesystem sync
- **Crash resilience** — the Zig PID 1 uses no heap allocations and auto-restarts
  the service manager with exponential backoff if it crashes

## Building

Requires Zig 0.15+ and Rust (2024 edition).

```sh
make              # Build dynamod-init (Zig) + all Rust binaries
make install      # Install to /usr (or DESTDIR=... PREFIX=...)
make test         # Run unit tests (Zig + Rust)
```

The systemd shim library (`libsystemd.so`) is built separately since it needs
the glibc target:

```sh
make rust-sdnotify    # Build libsystemd.so.0
make install-sdnotify # Install to /usr/lib/
```

## Try it in Docker

The fastest way to see dynamod in action — no VM or bootloader changes needed:

```sh
cd docker
docker compose up
```

Then in another terminal:

```sh
docker exec -it docker-dynamod-1 dynamodctl list
docker exec -it docker-dynamod-1 dynamodctl tree
```

See [docs/docker.md](docs/docker.md) for details.

## Quick start

**1. Define a service:**

```toml
# /etc/dynamod/services/myapp.toml
[service]
name = "myapp"
exec = ["/usr/bin/myapp", "--port", "8080"]

[restart]
policy = "permanent"
delay = "2s"

[readiness]
type = "tcp-port"
port = 8080

[dependencies]
requires = ["network"]
```

**2. Manage it:**

```sh
dynamodctl list              # See all services and their status
dynamodctl start myapp       # Start a service
dynamodctl status myapp      # Check its status (running, pid, supervisor)
dynamodctl restart myapp     # Restart it
dynamodctl tree              # View the full supervisor tree
dynamodctl shutdown reboot   # Reboot the system
```

## Booting from disk

dynamod supports the standard Linux boot flow: bootloader loads kernel + initramfs,
initramfs detects the root device, mounts it, and hands off to the real rootfs.

The kernel command line controls root device selection:

```
root=/dev/sda1 rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init
root=UUID=abcd-1234 rootfstype=btrfs rootwait rdinit=/sbin/dynamod-init
root=PARTUUID=xxxx rootwait rdinit=/sbin/dynamod-init
```

What happens during boot:

1. dynamod-init starts as PID 1 in the initramfs
2. Mounts `/proc`, `/sys`, `/dev`
3. Reads `/proc/cmdline`, finds `root=/dev/vda1`
4. Runs `mdev -s` to create device nodes
5. Mounts the ext4 partition on `/newroot`
6. Moves `/proc`, `/sys`, `/dev` into `/newroot`
7. Deletes initramfs contents to free RAM
8. `switch_root` + re-executes itself from the real rootfs
9. Normal boot continues: hostname, entropy, spawn svmgr, start services

To create a bootable disk image for testing:

```sh
sudo tools/mkimage.sh           # Creates test/alpine/build-disk/{disk.img,initramfs.gz}
sudo test/alpine/boot-disk.sh   # Boots it in QEMU
```

## Testing

```sh
make test                          # Unit tests (Zig + Rust)
make test-qemu                     # Minimal QEMU boot smoke test
make test-alpine                   # Full Alpine integration test
sudo make test-dbus                # D-Bus interface smoke test (logind, systemd1, hostname1)
sudo test/alpine/boot-disk.sh     # Initramfs-to-rootfs transition test
sudo test/alpine/boot-wayland.sh  # Sway Wayland compositor test
sudo test/alpine/boot-gnome.sh    # GNOME Shell desktop test
```

## Configuration

Services live in `/etc/dynamod/services/*.toml`, supervisors in
`/etc/dynamod/supervisors/*.toml`. See [docs/configuration.md](docs/configuration.md)
for the full reference.

## Documentation

- [Installation Guide](docs/installation.md) — building, packaging, kernel requirements,
  setting up dynamod as your init system
- [Architecture](docs/architecture.md) — how the system works, boot sequence,
  IPC protocol, supervisor trees, systemd-mimic layer
- [Configuration](docs/configuration.md) — every TOML field with defaults and examples
- [Docker](docs/docker.md) — running dynamod in a Docker container
- [Contributing](CONTRIBUTING.md) — how to contribute, code style, testing

## Project structure

```
dynamod/
├── zig/src/               # PID 1 init (Zig, ~2300 lines)
│   ├── main.zig           # Entry point, two-phase boot detection
│   ├── boot.zig           # Mount filesystems, hostname, entropy, machine-id
│   ├── cmdline.zig        # Kernel command line parser
│   ├── rootdev.zig        # Root device resolver (UUID, PARTUUID, LABEL)
│   ├── switchroot.zig     # Initramfs-to-rootfs switch_root
│   ├── event_loop.zig     # epoll main loop
│   ├── signal.zig         # signalfd handler
│   ├── shutdown.zig       # Shutdown: signal, unmount, sync, reboot
│   ├── child.zig          # Spawn/monitor svmgr
│   ├── reaper.zig         # Zombie reaping
│   ├── ipc.zig            # IPC message framing
│   ├── msgpack.zig        # MessagePack codec
│   ├── kmsg.zig           # Kernel log writer (/dev/kmsg)
│   └── constants.zig      # Paths and constants
├── rust/
│   ├── dynamod-svmgr/     # Service manager
│   ├── dynamod-common/    # Shared protocol + types
│   ├── dynamodctl/        # CLI tool
│   ├── dynamod-logd/      # Log daemon
│   ├── dynamod-logind/    # login1 D-Bus service (session/seat/device)
│   ├── dynamod-sd1bridge/ # systemd1 D-Bus bridge
│   ├── dynamod-hostnamed/ # hostname1 + timedate1 + locale1
│   └── dynamod-sdnotify/  # libsystemd.so shim (cdylib)
├── config/
│   ├── services/          # Service definitions (*.toml)
│   ├── supervisors/       # Supervisor tree definitions
│   └── dbus-1/            # D-Bus policy files
├── docker/
│   ├── Dockerfile         # Multi-stage build (Ubuntu builder → Alpine runtime)
│   ├── docker-compose.yml # One-command demo
│   └── services/          # Docker-adapted service configs
├── tools/
│   └── mkimage.sh         # Bootable disk image builder
├── test/
│   ├── qemu/              # Minimal QEMU boot test
│   └── alpine/            # Alpine-based integration tests
│       ├── build-test.sh      # Headless serial test
│       ├── boot-gui.sh        # Graphical VGA test
│       ├── test-dbus.sh       # D-Bus interface smoke test
│       ├── boot-disk.sh       # Initramfs → ext4 rootfs test
│       ├── boot-wayland.sh    # Sway Wayland test
│       └── boot-gnome.sh      # GNOME Shell test
└── docs/
    ├── architecture.md
    ├── configuration.md
    └── docker.md
```

## License

MIT
