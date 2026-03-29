# Configuration Reference

Services are TOML files in `/etc/dynamod/services/`.
Supervisors are TOML files in `/etc/dynamod/supervisors/`.

Each service file defines one service. Only `name` and `exec` are required —
everything else has sensible defaults.

## Service configuration

### `[service]` — Identity and command

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Unique service name |
| `supervisor` | string | `"root"` | Which supervisor manages this service |
| `exec` | string[] | **required** | Command and arguments |
| `workdir` | string | — | Working directory before exec |
| `type` | string | `"simple"` | How the service runs (see below) |
| `environment` | map | `{}` | Environment variables: `{ KEY = "value" }` |
| `environment-file` | string | — | Path to a file with `KEY=VALUE` lines |

**Service types:**

- `simple` — a long-running foreground process (most services)
- `oneshot` — runs once and exits; considered "ready" when it exits successfully
- `forking` — the process forks into the background (legacy daemons)
- `notify` — sends `READY=1` to `$NOTIFY_SOCKET` when ready (sd_notify compatible)

### `[service.user]` — Privilege dropping

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `user` | string | — | Run as this user (name or UID) |
| `group` | string | — | Run as this group (name or GID) |
| `supplementary-groups` | string[] | `[]` | Additional groups |

### `[restart]` — What happens when it exits

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `policy` | string | `"permanent"` | When to restart (see below) |
| `delay` | duration | `"1s"` | Wait this long before restarting |
| `max-restarts` | int | `5` | Max restarts before giving up |
| `max-restart-window` | duration | `"60s"` | Time window for counting restarts |

**Restart policies:**

- `permanent` — always restart, no matter how it exited
- `transient` — restart only on abnormal exit (non-zero code or killed by signal)
- `temporary` — never restart (good for oneshot tasks)

If `max-restarts` is exceeded within `max-restart-window`, the supervisor
escalates the failure to its parent.

### `[readiness]` — When is it "ready"?

Dependencies wait for a service to be ready, not just started. This section
controls how readiness is detected.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `type` | string | `"none"` | Detection method (see below) |
| `port` | int | — | TCP port to poll (for `tcp-port`) |
| `check-exec` | string[] | — | Health check command (for `exec`) |
| `timeout` | duration | `"30s"` | Give up after this long |

**Readiness types:**

- `none` — ready immediately after the process starts
- `notify` — service sends `READY=1` to `$NOTIFY_SOCKET` (sd_notify protocol)
- `tcp-port` — dynamod polls the specified TCP port until it accepts connections
- `exec` — dynamod runs a command periodically; ready when it exits 0
- `fd` — service writes a byte to the file descriptor in `$DYNAMOD_READY_FD`

### `[dependencies]` — Startup ordering

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `requires` | string[] | `[]` | Hard deps — must be ready; blocked if they fail |
| `wants` | string[] | `[]` | Soft deps — ordering only if present, won't block |
| `after` | string[] | `[]` | Start after these services (ordering, no failure propagation) |
| `before` | string[] | `[]` | Start before these services |

The difference between `requires` and `after`:

- `requires = ["database"]` — if database fails to start, this service is blocked
- `after = ["database"]` — start after database, but if database fails, start anyway

### `[cgroup]` — Resource limits (cgroups v2)

| Field | Type | Description |
|-------|------|-------------|
| `memory-max` | size | Hard memory limit — OOM kill above this |
| `memory-high` | size | Soft limit — kernel applies reclaim pressure |
| `cpu-weight` | int | Relative CPU share, 1–10000 (default 100) |
| `cpu-max` | string | CPU bandwidth: `"quota period"` (e.g. `"200000 100000"` = 200%) |
| `pids-max` | int | Maximum number of processes |
| `io-weight` | int | Relative I/O priority, 1–10000 (default 100) |

### `[namespace]` — Process isolation

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enable` | string[] | `[]` | Namespaces to activate: `pid`, `mnt`, `net`, `uts`, `ipc`, `user`, `cgroup` |
| `bind-mounts` | object[] | `[]` | Each: `{ source, target, writable }` |
| `private-tmp` | bool | `false` | Give the service its own `/tmp` |
| `protect-system` | string | — | `"strict"` (read-only root) or `"full"` (read-only `/usr`, `/boot`) |

### `[shutdown]` — How to stop it

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `stop-signal` | string | `"SIGTERM"` | Signal to send for graceful stop |
| `stop-timeout` | duration | `"10s"` | Time to wait before sending SIGKILL |
| `stop-exec` | string[] | — | Run this command instead of sending a signal |

## Supervisor configuration

Supervisors define the tree structure and restart strategies.

### `[supervisor]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Unique supervisor name |
| `parent` | string | `"root"` | Parent supervisor in the tree |
| `strategy` | string | `"one-for-one"` | Restart strategy (see below) |

### `[restart]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max-restarts` | int | `10` | Max restarts before the supervisor itself fails |
| `max-restart-window` | duration | `"300s"` | Time window for counting |

**Restart strategies:**

- `one-for-one` — only the crashed child restarts. Best for independent services.
- `one-for-all` — all children stop (reverse order) then restart (start order).
  Use when children are tightly coupled.
- `rest-for-one` — the crashed child and all children started after it restart.
  Use when later children depend on earlier ones.

## Value formats

### Duration

`"30s"` (seconds), `"5m"` (minutes), `"1h"` (hours), or a plain number
(interpreted as seconds).

### Size

`"512M"` (mebibytes), `"2G"` (gibibytes), `"1024K"` (kibibytes), `"max"`
(unlimited), or a plain number (interpreted as bytes).

## Full example

```toml
# /etc/dynamod/services/postgresql.toml

[service]
name = "postgresql"
supervisor = "data-stores"
exec = ["/usr/bin/postgres", "-D", "/var/lib/postgresql/data"]
workdir = "/var/lib/postgresql"
type = "simple"
environment = { PGDATA = "/var/lib/postgresql/data", LC_ALL = "C.UTF-8" }

[service.user]
user = "postgres"
group = "postgres"

[restart]
policy = "permanent"
delay = "3s"
max-restarts = 5
max-restart-window = "120s"

[readiness]
type = "tcp-port"
port = 5432
timeout = "45s"

[dependencies]
requires = ["network"]
wants = ["syslog"]

[cgroup]
memory-max = "2G"
memory-high = "1536M"
cpu-weight = 200
pids-max = 256

[namespace]
enable = ["pid", "mnt"]
private-tmp = true
protect-system = "strict"
bind-mounts = [
    { source = "/var/lib/postgresql", target = "/var/lib/postgresql", writable = true },
    { source = "/run/postgresql", target = "/run/postgresql", writable = true },
]

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "30s"
```

## Minimal example

The simplest possible service:

```toml
# /etc/dynamod/services/hello.toml
[service]
name = "hello"
exec = ["/usr/bin/echo", "hello world"]
type = "oneshot"

[restart]
policy = "temporary"
```
