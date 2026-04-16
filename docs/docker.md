# Docker

Run dynamod as PID 1 inside a Docker container. This is the fastest way to
try dynamod without setting up a VM or modifying your bootloader.

## Quick start

```sh
cd docker
docker compose up
```

This builds the image from source (first run takes a few minutes) and starts
an Alpine container with dynamod as PID 1. You'll see the boot log in your
terminal:

```
INFO dynamod-svmgr starting
INFO loaded 8 service definition(s)
INFO loaded 3 supervisor definition(s)
INFO starting services via dependency frontier
INFO oneshot 'bootmisc' completed (code 0)
INFO oneshot 'hostname' completed (code 0)
INFO oneshot 'network' completed (code 0)
INFO startup complete: 8 ready, 0 blocked
```

## Interacting with the container

Open a second terminal and use `dynamodctl` via `docker exec`:

```sh
docker exec -it docker-dynamod-1 dynamodctl list
docker exec -it docker-dynamod-1 dynamodctl tree
docker exec -it docker-dynamod-1 dynamodctl status syslog
docker exec -it docker-dynamod-1 /bin/sh     # Get a shell
```

To stop the container cleanly:

```sh
docker compose down
```

Docker sends SIGTERM to dynamod-init, which performs a full shutdown sequence:
stop all services in reverse dependency order, SIGKILL stragglers, sync
filesystems, then exit.

## What's included

The Docker image ships with a curated set of services that work inside
containers. Hardware-dependent services (fsck, udev, kernel modules) are
excluded since Docker provides its own device and filesystem layer.

### Services

| Service | Type | Supervisor | What it does |
|---------|------|------------|-------------|
| `bootmisc` | oneshot | early-boot | Creates `/tmp`, `/var/log`, and other standard directories |
| `hostname` | oneshot | early-boot | Sets the container hostname from `/etc/hostname` |
| `network` | oneshot | early-boot | Brings up the loopback interface |
| `sysctl` | oneshot | early-boot | Applies kernel parameters from `/etc/sysctl.conf` |
| `machine-id` | oneshot | early-boot | Generates `/etc/machine-id` if missing |
| `dynamod-logd` | simple | early-boot | Log collection daemon |
| `syslog` | simple | root | BusyBox syslogd writing to `/var/log/messages` |
| `agetty-tty1` | simple | root | Getty on tty1 (for `docker attach`) |

### Supervisors

The standard supervisor tree is included: `root` (one-for-one), `early-boot`
(one-for-all, child of root), and `desktop` (one-for-one, child of root).
The desktop supervisor has no services assigned in the Docker config.

### Excluded services

These are in the main `config/services/` but not in the Docker image:

- `fsck`, `remount-root-rw`, `fstab-mount` — root is already writable in containers
- `modules-load` — no kernel module access
- `udev`, `udev-coldplug`, `mdev-coldplug` — Docker manages `/dev`
- `dbus`, `dynamod-logind`, `dynamod-sd1bridge`, `dynamod-hostnamed` — desktop D-Bus services
- `sshd`, `nginx`, `postgresql` — optional application services

## Adding your own services

Mount a custom service config into the container:

```yaml
services:
  dynamod:
    build:
      context: ..
      dockerfile: docker/Dockerfile
    privileged: true
    hostname: dynamod-docker
    tty: true
    stdin_open: true
    stop_signal: SIGTERM
    stop_grace_period: 10s
    tmpfs:
      - /tmp
    volumes:
      - ./myapp.toml:/etc/dynamod/services/myapp.toml:ro
```

Where `myapp.toml` is a standard dynamod service definition:

```toml
[service]
name = "myapp"
exec = ["/usr/bin/myapp"]

[restart]
policy = "permanent"
delay = "2s"

[dependencies]
after = ["network"]
```

After starting the container, verify your service is loaded:

```sh
docker exec docker-dynamod-1 dynamodctl list
docker exec docker-dynamod-1 dynamodctl status myapp
```

## How it works

### Boot flow

1. Docker starts `/sbin/dynamod-init` as PID 1
2. Init mounts pseudo-filesystems (most return EBUSY since Docker already
   mounts them — this is handled gracefully)
3. Init creates `/run/dynamod`, sets hostname, seeds entropy
4. Init spawns `dynamod-svmgr`
5. Svmgr loads all TOML configs, builds the dependency graph, starts
   services via the frontier algorithm
6. Init enters its epoll loop: signal handling, heartbeats, zombie reaping

### Why `privileged: true`?

dynamod-init mounts tmpfs on `/run` and cgroup2 on `/sys/fs/cgroup`, and
dynamod-svmgr creates per-service cgroups for resource isolation. These
operations require `CAP_SYS_ADMIN`. Running with `privileged: true` is the
simplest way to grant the necessary permissions.

If you want to minimize privileges, you can try replacing `privileged: true`
with specific capabilities:

```yaml
cap_add:
  - SYS_ADMIN
  - NET_ADMIN
security_opt:
  - apparmor:unconfined
```

Note that cgroup management may still fail depending on your Docker daemon's
cgroup namespace configuration.

## Dockerfile structure

The image uses a multi-stage build:

**Stage 1 (builder):** Ubuntu 24.04 with Zig 0.15.2 and Rust (musl target).
Runs `make all` to produce statically-linked binaries.

**Stage 2 (runtime):** Alpine 3.21 with only `iproute2` added (for the
`ip link set lo up` command in the network service). All dynamod binaries
are copied from the builder stage. Total image size is ~30 MB.

## Troubleshooting

### syslog keeps restarting

If `/dev/log` is a broken symlink (common when the host uses systemd), the
Docker syslog config handles this by removing the symlink before starting
syslogd. If you replace the syslog config, make sure to include:

```toml
exec = ["/bin/sh", "-c", "rm -f /dev/log; exec /sbin/syslogd -n -O /var/log/messages"]
```

### Cgroup warnings in the logs

You may see warnings like "failed to apply cgroup limits" or "failed to add
pid to cgroup". These are non-fatal — the service starts normally, just
without cgroup resource limits. This happens when Docker's cgroup namespace
doesn't allow writes to the cgroup hierarchy. To avoid the warnings, don't
set `[cgroup]` limits in your Docker service configs.

### Container exits immediately

Check that you're running with `privileged: true` (or equivalent capabilities).
Without it, dynamod-init fails to mount essential filesystems and halts.
