# Dynamod Configuration Reference

Services are configured via TOML files in `/etc/dynamod/services/`.
Supervisors are configured in `/etc/dynamod/supervisors/`.

## Service Configuration

Each service is a single `.toml` file. Only `[service].name` and
`[service].exec` are required; all other fields have sensible defaults.

### `[service]` — Service identity and command

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Unique service name |
| `supervisor` | string | `"root"` | Parent supervisor |
| `exec` | string[] | **required** | Command and arguments |
| `workdir` | string | — | Working directory |
| `type` | string | `"simple"` | `simple`, `oneshot`, `forking`, `notify` |
| `environment` | map | `{}` | Environment variables |
| `environment-file` | string | — | Path to env file (KEY=VALUE lines) |

### `[service.user]` — Privilege dropping

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `user` | string | — | User name or UID |
| `group` | string | — | Group name or GID |
| `supplementary-groups` | string[] | `[]` | Additional groups |

### `[restart]` — Restart behavior

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `policy` | string | `"permanent"` | `permanent`, `transient`, `temporary` |
| `delay` | string | `"1s"` | Delay before restart |
| `max-restarts` | int | `5` | Max restarts within the window |
| `max-restart-window` | string | `"60s"` | Time window for counting restarts |

**Policies:**
- `permanent` — Always restart, regardless of exit reason
- `transient` — Restart only on abnormal exit (non-zero code or signal kill)
- `temporary` — Never restart

### `[readiness]` — Readiness detection

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `type` | string | `"none"` | `none`, `notify`, `tcp-port`, `exec`, `fd` |
| `port` | int | — | TCP port (for `tcp-port` type) |
| `check-exec` | string[] | — | Health check command (for `exec` type) |
| `timeout` | string | `"30s"` | Max time to wait for readiness |

**Types:**
- `none` — Ready immediately after exec
- `notify` — sd_notify compatible (`READY=1` on `$NOTIFY_SOCKET`)
- `tcp-port` — Poll until TCP port accepts connections
- `exec` — Run a command; ready when it exits 0
- `fd` — Service writes a byte to a passed file descriptor

### `[dependencies]` — Startup ordering

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `requires` | string[] | `[]` | Hard dependencies (must be READY; blocks if failed) |
| `wants` | string[] | `[]` | Soft dependencies (ordering only if present) |
| `after` | string[] | `[]` | Start after (ordering, no hard dependency) |
| `before` | string[] | `[]` | Start before these services |

### `[cgroup]` — Resource limits (cgroups v2)

| Field | Type | Description |
|-------|------|-------------|
| `memory-max` | string | Hard memory limit (e.g. `"2G"`, `"512M"`) |
| `memory-high` | string | Soft memory limit (reclaim pressure) |
| `cpu-weight` | int | CPU weight 1-10000 (default 100) |
| `cpu-max` | string | CPU bandwidth `"quota period"` (e.g. `"200000 100000"`) |
| `pids-max` | int | Maximum number of processes |
| `io-weight` | int | I/O weight 1-10000 (default 100) |

### `[namespace]` — Isolation

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enable` | string[] | `[]` | Namespaces: `pid`, `mnt`, `net`, `uts`, `ipc`, `user`, `cgroup` |
| `bind-mounts` | object[] | `[]` | Bind mount entries (`source`, `target`, `writable`) |
| `private-tmp` | bool | `false` | Mount private tmpfs on `/tmp` |
| `protect-system` | string | — | `"strict"` (read-only `/`) or `"full"` (read-only `/usr`, `/boot`) |

### `[shutdown]` — Stop behavior

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `stop-signal` | string | `"SIGTERM"` | Signal to send for graceful stop |
| `stop-timeout` | string | `"10s"` | Timeout before SIGKILL escalation |
| `stop-exec` | string[] | — | Command to run instead of signal |

## Supervisor Configuration

Supervisor files define the tree structure and restart strategies.

### `[supervisor]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Unique supervisor name |
| `parent` | string | `"root"` | Parent supervisor |
| `strategy` | string | `"one-for-one"` | `one-for-one`, `one-for-all`, `rest-for-one` |

### `[restart]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max-restarts` | int | `10` | Max restarts before supervisor fails |
| `max-restart-window` | string | `"300s"` | Time window for counting |

## Duration Format

All duration fields accept: `"30s"` (seconds), `"5m"` (minutes), `"1h"` (hours),
or a plain number interpreted as seconds.

## Size Format

Memory fields accept: `"512M"` (mebibytes), `"2G"` (gibibytes), `"1024K"` (kibibytes),
`"max"` (unlimited), or a plain number interpreted as bytes.

## Full Example

```toml
# /etc/dynamod/services/postgresql.toml

[service]
name = "postgresql"
supervisor = "data-stores"
exec = ["/usr/bin/postgres", "-D", "/var/lib/postgresql/data"]
workdir = "/var/lib/postgresql"
type = "simple"
environment = { PGDATA = "/var/lib/postgresql/data" }

[service.user]
user = "postgres"
group = "postgres"

[restart]
policy = "permanent"
delay = "2s"
max-restarts = 5
max-restart-window = "60s"

[readiness]
type = "tcp-port"
port = 5432
timeout = "30s"

[dependencies]
requires = ["filesystem.local", "network.loopback"]
wants = ["syslog"]

[cgroup]
memory-max = "2G"
cpu-weight = 200
pids-max = 256

[namespace]
enable = ["pid", "mnt"]
private-tmp = true
protect-system = "strict"

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "30s"
```
