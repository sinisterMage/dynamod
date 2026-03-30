# Installation Guide

This guide covers building dynamod from source and installing it on your system.
dynamod can run as the primary init system or be tested in a QEMU VM.

## Build from source

### Prerequisites

| Tool | Version | What it's for |
|------|---------|--------------|
| Zig | 0.15+ | Building dynamod-init (PID 1) |
| Rust | 2024 edition | Building service manager and tools |
| musl target | — | Static linking (`rustup target add x86_64-unknown-linux-musl`) |
| make | any | Build orchestration |

Optional for testing:

| Tool | What it's for |
|------|--------------|
| QEMU | Integration tests (boot dynamod as PID 1 in a VM) |
| cpio, gzip | Building initramfs images for QEMU tests |
| sfdisk, mkfs.ext4, losetup | Building disk images for rootfs tests |

### Building

```sh
git clone https://github.com/sinisterMage/dynamod.git
cd dynamod
make
```

This produces:

| Binary | Location |
|--------|----------|
| `dynamod-init` | `zig/zig-out/bin/dynamod-init` |
| `dynamod-svmgr` | `rust/target/x86_64-unknown-linux-musl/release/dynamod-svmgr` |
| `dynamodctl` | `rust/target/x86_64-unknown-linux-musl/release/dynamodctl` |
| `dynamod-logd` | `rust/target/x86_64-unknown-linux-musl/release/dynamod-logd` |
| `dynamod-logind` | `rust/target/x86_64-unknown-linux-musl/release/dynamod-logind` |
| `dynamod-sd1bridge` | `rust/target/x86_64-unknown-linux-musl/release/dynamod-sd1bridge` |
| `dynamod-hostnamed` | `rust/target/x86_64-unknown-linux-musl/release/dynamod-hostnamed` |

All binaries are statically linked and have no runtime dependencies.

### Installing

```sh
sudo make install           # Installs to /usr/
sudo make install-alpine    # Also installs all service configs
```

Or install to a custom prefix:

```sh
sudo make install DESTDIR=/path/to/rootfs PREFIX=/usr
```

## Distro packages

Pre-built packages for local installation (not in distro repos yet):

### Arch Linux

```sh
cd dist/arch
makepkg -si
```

### Alpine Linux

```sh
cd dist/alpine
abuild -r
sudo apk add --allow-untrusted ~/packages/*/dynamod-*.apk
```

### Gentoo

Create a local overlay and copy the ebuild:

```sh
sudo mkdir -p /var/db/repos/local/sys-apps/dynamod
sudo cp dist/gentoo/dynamod-0.1.0.ebuild /var/db/repos/local/sys-apps/dynamod/
cd /var/db/repos/local/sys-apps/dynamod
sudo ebuild dynamod-0.1.0.ebuild manifest
sudo emerge --ask sys-apps/dynamod
```

### Void Linux

```sh
# From within a void-packages clone:
cp -r dist/void srcpkgs/dynamod
./xbps-src pkg dynamod
sudo xbps-install -R hostdir/binpkgs dynamod
```

## What gets installed

```
/usr/sbin/dynamod-init              # PID 1 binary
/usr/lib/dynamod/dynamod-svmgr     # Service manager
/usr/lib/dynamod/dynamod-logd      # Log daemon
/usr/lib/dynamod/dynamod-logind    # login1 D-Bus service
/usr/lib/dynamod/dynamod-sd1bridge # systemd1 D-Bus bridge
/usr/lib/dynamod/dynamod-hostnamed # hostname1/timedate1/locale1
/usr/bin/dynamodctl                 # CLI tool
/etc/dynamod/services/*.toml        # Service definitions
/etc/dynamod/supervisors/*.toml     # Supervisor tree definitions
/usr/share/dbus-1/system.d/*.conf   # D-Bus policy files
```

## Setting up dynamod as your init system

**Warning:** Replacing your init system on a running machine can make it
unbootable. Test in a VM first!

### Option 1: QEMU testing (recommended to start)

The fastest way to try dynamod:

```sh
make
make test-alpine     # Boots Alpine + dynamod in QEMU, serial console
```

For graphical testing:

```sh
sudo test/alpine/boot-wayland.sh   # Sway desktop
sudo test/alpine/boot-gnome.sh     # GNOME Shell desktop
```

For disk-based boot (initramfs -> ext4 rootfs):

```sh
sudo test/alpine/boot-disk.sh
```

### Option 2: Install into a root filesystem

Prepare a root filesystem (ext4 partition, Alpine/Void/Gentoo base install),
then install dynamod into it:

```sh
# Mount your target root
sudo mount /dev/sdX1 /mnt

# Install dynamod
sudo make install DESTDIR=/mnt PREFIX=/usr
sudo make install-alpine DESTDIR=/mnt PREFIX=/usr

# Create required directories
sudo mkdir -p /mnt/var/lib/dynamod /mnt/var/log/dynamod

# Configure your bootloader to use dynamod
# GRUB example (add to /mnt/boot/grub/grub.cfg):
#   linux /boot/vmlinuz root=/dev/sdX1 rootfstype=ext4 init=/sbin/dynamod-init
#   initrd /boot/initramfs.gz
```

### Option 3: Boot with initramfs (for real hardware)

For a proper boot chain with root device detection:

1. Build an initramfs with dynamod-init and busybox:

```sh
sudo tools/mkimage.sh /tmp/dynamod-boot
# This creates initramfs.gz and disk.img
# Use just the initramfs.gz for real hardware
```

2. Copy the initramfs and configure your bootloader:

```
# GRUB entry
menuentry "dynamod" {
    linux /boot/vmlinuz root=UUID=your-uuid rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init
    initrd /boot/dynamod-initramfs.gz
}
```

The initramfs contains only dynamod-init and busybox (~3MB). It detects
the root device, mounts it, and switch_roots to the real filesystem.

## Kernel requirements

dynamod needs a fairly standard Linux kernel. The key configs:

### Required

```
CONFIG_DEVTMPFS=y          # /dev auto-population
CONFIG_TMPFS=y             # /run, /tmp
CONFIG_PROC_FS=y           # /proc
CONFIG_SYSFS=y             # /sys
CONFIG_CGROUPS=y           # cgroups v2
CONFIG_UNIX=y              # Unix domain sockets (IPC)
CONFIG_FILE_LOCKING=y      # flock() for Wayland socket locks
CONFIG_SIGNALFD=y          # signalfd for signal handling
CONFIG_EPOLL=y             # epoll for event loop
```

### For disk boot

```
CONFIG_EXT4_FS=y           # or your rootfs type
CONFIG_BLK_DEV_INITRD=y   # initramfs support
```

Storage driver for your hardware:
```
CONFIG_VIRTIO_BLK=y        # QEMU virtio (for testing)
CONFIG_SATA_AHCI=y         # SATA drives
CONFIG_BLK_DEV_NVME=y     # NVMe SSDs
```

### For Wayland/GNOME desktop

```
CONFIG_DRM=y
CONFIG_DRM_VIRTIO_GPU=y   # for QEMU testing
CONFIG_DRM_KMS_HELPER=y
CONFIG_PCI=y
CONFIG_VIRTIO_MENU=y
CONFIG_VIRTIO_PCI=y
CONFIG_INPUT_EVDEV=y
```

## Writing service configs

See [configuration.md](configuration.md) for the full TOML reference.

Quick example — a web server:

```toml
# /etc/dynamod/services/nginx.toml
[service]
name = "nginx"
exec = ["/usr/sbin/nginx", "-g", "daemon off;"]

[restart]
policy = "permanent"
delay = "2s"

[readiness]
type = "tcp-port"
port = 80

[dependencies]
requires = ["network"]
after = ["syslog"]

[cgroup]
memory-max = "512M"
pids-max = 64
```

## Troubleshooting

### dynamod-init panics / crashes on boot

This shouldn't happen (the Zig code has no panics), but if it does:

- Check the kernel log: the init writes to `/dev/kmsg` with a `dynamod-init:` prefix
- Verify you're using a supported kernel (see kernel requirements above)
- Make sure `/proc`, `/sys`, `/dev` mount points exist in your rootfs

### Services won't start

- Check `dynamodctl list` for status
- Check `dynamodctl status <name>` for error details
- Look at svmgr's stderr output (it logs to the console)
- Verify the TOML config is valid: `dynamodctl` will show parse errors

### Wayland compositor can't access GPU

- Make sure the kernel has DRM support (`CONFIG_DRM=y` + your GPU driver)
- For QEMU: need `CONFIG_DRM_VIRTIO_GPU=y`
- For sway: set `LIBSEAT_BACKEND=seatd` and start `seatd`
- For GNOME: needs udevd running + session tracking files at `/run/systemd/sessions/`

### D-Bus services not registering

- Make sure `dbus-daemon` is running (it should be a dynamod service)
- Check D-Bus policy files are installed at `/usr/share/dbus-1/system.d/`
- On Alpine: `/var/run` must be a symlink to `/run` (libelogind needs this)
