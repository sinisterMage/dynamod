# Contributing to dynamod

Thanks for your interest in contributing to dynamod! This document covers
everything you need to get started.

## Getting started

### Prerequisites

- **Zig 0.15+** — for dynamod-init (the PID 1)
- **Rust (2024 edition)** — for the service manager and tools
- **Linux kernel headers** — dynamod uses raw Linux syscalls
- **QEMU** — for integration testing (optional but very helpful)

### Building

```sh
git clone https://github.com/sinisterMage/dynamod.git
cd dynamod
make          # Builds both Zig and Rust components
make test     # Runs unit tests
```

The Rust workspace is configured to cross-compile to `x86_64-unknown-linux-musl`
for static binaries. Make sure you have the musl target installed:

```sh
rustup target add x86_64-unknown-linux-musl
```

### Running tests

```sh
make test              # Unit tests (Zig + Rust)
make test-qemu         # Minimal QEMU smoke test
make test-alpine       # Full Alpine integration test
```

For desktop and D-Bus tests (require root for chroot/mount):

```sh
sudo make test-dbus               # D-Bus interface smoke test
sudo test/alpine/boot-disk.sh    # Initramfs-to-rootfs transition
sudo test/alpine/boot-wayland.sh # Sway Wayland compositor
sudo test/alpine/boot-gnome.sh   # GNOME Shell desktop
```

## Project structure

dynamod is split into two languages by design:

- **Zig** (`zig/src/`) — PID 1 init process. Minimal, no heap allocations, can't
  crash. If you're touching this, you're working on the most safety-critical
  part of the system.

- **Rust** (`rust/`) — Everything else: service manager, CLI, logging, D-Bus
  interfaces. This is where most feature work happens.

Key directories:

| Directory | What's there |
|-----------|-------------|
| `zig/src/` | PID 1: boot, shutdown, IPC, signal handling, switch_root |
| `rust/dynamod-svmgr/` | Service manager: supervisor trees, dependency DAG, process spawning |
| `rust/dynamod-common/` | Shared types and IPC protocol |
| `rust/dynamodctl/` | CLI tool |
| `rust/dynamod-logind/` | login1 D-Bus service |
| `rust/dynamod-sd1bridge/` | systemd1 D-Bus bridge |
| `rust/dynamod-hostnamed/` | hostname1/timedate1/locale1 |
| `config/` | Example TOML service and supervisor configs |
| `test/` | QEMU-based integration tests |

## What to work on

### Good first issues

Look for issues labeled `good first issue`. These are typically:

- Adding a missing D-Bus property or method
- Improving error messages
- Adding a new example service config
- Documentation improvements

### Larger contributions

If you're planning something bigger (new feature, architectural change),
please open an issue first to discuss the approach. This saves everyone time.

## Code style

### Zig

- Follow the existing patterns: raw syscalls, fixed buffers, `?kmsg` for logging
- No heap allocations in PID 1 code
- All functions that can fail should take a `?kmsg` parameter for error logging
- Keep it minimal — dynamod-init is currently ~2300 lines across 14 files; avoid unnecessary growth

### Rust

- Use `cargo fmt --all` before committing
- Follow the existing workspace patterns (workspace-inherited versions, etc.)
- New crates go in `rust/` and are added to the workspace `Cargo.toml`
- The `dynamod-sdnotify` crate is special — it's a cdylib excluded from the
  default musl build (see the workspace Cargo.toml comments)

### TOML configs

- Service configs should be self-documenting
- Include sensible defaults — most fields should be optional
- Follow the naming convention: `kebab-case` for fields, plain names for services

## Commit messages

Write clear commit messages that explain *why*, not just *what*. One-liners
are fine for small changes. For bigger changes, use a blank line after the
summary and explain the context.

Good:
```
add TakeDevice fd passing for Wayland compositors

Mutter and sway need to acquire DRM/input device fds from logind
via the TakeDevice D-Bus method. This implements the fd passing
using zbus's OwnedFd type over SCM_RIGHTS.
```

Fine for small stuff:
```
fix busctl -> dbus-send in Alpine test scripts
```

## Pull requests

1. Fork the repo and create a branch from `main`
2. Make your changes
3. Run `make test` and make sure everything passes
4. If you changed Zig code, run `cd zig && zig build test`
5. If you changed Rust code, run `cd rust && cargo test --workspace`
6. Open a PR with a clear description of what you changed and why

## Testing

### Unit tests

Zig tests are in the source files (look for `test "..."` blocks).
Rust tests are in `#[cfg(test)]` modules.

### Integration tests

The QEMU tests boot a real Linux system with dynamod as PID 1. They're the
most thorough way to verify changes. If you're changing boot, shutdown, or
service management behavior, please run the relevant QEMU test.

### Adding a test

- For a new QEMU test: add a script in `test/alpine/` following the existing patterns
- For a new unit test: add it in the relevant source file
- For a new service config: add it in `config/services/` and test it in QEMU

## Architecture decisions

Some things are intentional and shouldn't change without very good reason:

- **PID 1 is in Zig, not Rust** — Zig gives us zero-cost syscalls and no
  runtime. PID 1 must never crash.
- **MessagePack for IPC, not protobuf/JSON** — compact, simple, no codegen needed.
  The Zig decoder is ~300 lines.
- **TOML for config, not YAML/JSON** — human-friendly, unambiguous, good error messages.
- **No systemd code** — all systemd-mimic interfaces are clean-room implementations
  from freedesktop.org specs to avoid GPL infection. This is a hard requirement.
- **Static linking (musl)** — binaries work on any Linux distro without dependencies.
  The `dynamod-sdnotify` cdylib is the only exception (needs glibc for .so).

## License

dynamod is MIT licensed. By contributing, you agree that your contributions
will be licensed under MIT.

## Questions?

Open an issue! There are no dumb questions, especially for a project this young.
