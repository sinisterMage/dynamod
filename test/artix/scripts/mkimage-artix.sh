#!/bin/sh
# Create a bootable GPT disk image with GRUB + Artix Linux + dynamod.
#
# Produces a complete bootable disk image that QEMU can boot from cold
# (SeaBIOS -> GRUB -> kernel -> initramfs -> switch_root -> full userspace
# with eudev + sway desktop).
#
# Usage:
#   sudo test/artix/scripts/mkimage-artix.sh [output-dir]
#
# Environment variables:
#   KERNEL       - Path to bzImage (auto-detected if unset)
#   DISK_SIZE_MB - Disk image size in MB (default: 4096)
#
# Prerequisites:
#   - pacstrap (from arch-install-scripts; available on Arch, CachyOS, Artix, etc.)
#   - sfdisk, mkfs.ext4, losetup, mount (util-linux)
#   - grub-install (grub / grub2 / grub-pc-bin)
#   - cpio, gzip
#   - Root privileges (sudo)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
OUTPUT_DIR="${1:-$TEST_DIR/build-full}"
DISK_SIZE_MB="${DISK_SIZE_MB:-4096}"

ZIG_OUT="$PROJECT_ROOT/zig/zig-out/bin"
CARGO_OUT="$(if [ -d "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release" ]; then
    echo "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release"
else
    echo "$PROJECT_ROOT/rust/target/release"
fi)"

CACHE_DIR="$TEST_DIR/.cache"
LOOP=""
MNT=""

# ============================================================
# Cleanup trap
# ============================================================
cleanup() {
    set +e
    if [ -n "$MNT" ]; then
        umount "$MNT/proc" 2>/dev/null
        umount "$MNT/dev"  2>/dev/null
        umount "$MNT/sys"  2>/dev/null
        umount "$MNT"      2>/dev/null
    fi
    if [ -n "$LOOP" ]; then
        losetup -d "$LOOP" 2>/dev/null
    fi
}
trap cleanup EXIT

# ============================================================
# Checks
# ============================================================
echo "=== dynamod Artix Linux Disk Image Builder ==="
echo "Output: $OUTPUT_DIR"
echo "Disk:   ${DISK_SIZE_MB}MB"
echo ""

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script requires root (for losetup, mount, chroot, grub-install)."
    echo "  Run: sudo $0 $*"
    exit 1
fi

if ! command -v pacstrap >/dev/null 2>&1; then
    echo "ERROR: pacstrap not found."
    echo "  Install arch-install-scripts: pacman -S arch-install-scripts"
    exit 1
fi

GRUB_INSTALL=""
for cmd in grub-install grub2-install; do
    if command -v "$cmd" >/dev/null 2>&1; then
        GRUB_INSTALL="$cmd"
        break
    fi
done

for cmd in sfdisk mkfs.ext4 losetup cpio gzip; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: $cmd not found. Install util-linux, cpio, and gzip."
        exit 1
    fi
done

if [ -z "$GRUB_INSTALL" ]; then
    echo "ERROR: grub-install not found."
    echo "  Install one of: grub (Arch/Artix), grub2 (Fedora), grub-pc-bin (Debian/Ubuntu)"
    exit 1
fi

# Find kernel
if [ -z "$KERNEL" ]; then
    for k in \
        "$PROJECT_ROOT/linux-6.19.10/arch/x86/boot/bzImage" \
        /boot/vmlinuz-* \
        ; do
        if [ -f "$k" ]; then
            KERNEL="$k"
            break
        fi
    done
fi

if [ -z "$KERNEL" ] || [ ! -f "$KERNEL" ]; then
    echo "ERROR: No kernel found. Set KERNEL=/path/to/bzImage"
    exit 1
fi

echo "Kernel:       $KERNEL"
echo "grub-install: $GRUB_INSTALL"
echo ""

# Check dynamod binaries
for bin in "$ZIG_OUT/dynamod-init" \
           "$CARGO_OUT/dynamod-svmgr" \
           "$CARGO_OUT/dynamodctl" \
           "$CARGO_OUT/dynamod-logd" \
           "$CARGO_OUT/dynamod-logind" \
           "$CARGO_OUT/dynamod-sd1bridge" \
           "$CARGO_OUT/dynamod-hostnamed"; do
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found. Run 'make' first."
        exit 1
    fi
done

# ============================================================
# Prepare output directory
# ============================================================
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR" "$CACHE_DIR"

# ============================================================
# Part 1: Create GPT disk image
# ============================================================
echo "Creating ${DISK_SIZE_MB}MB disk image..."
dd if=/dev/zero of="$OUTPUT_DIR/disk.img" bs=1M count="$DISK_SIZE_MB" status=none

echo "Partitioning (GPT + BIOS boot)..."
sfdisk "$OUTPUT_DIR/disk.img" --quiet <<EOF
label: gpt
size=2M, type=21686148-6449-6E6F-744E-656564454649
type=linux
EOF

LOOP=$(losetup --find --show --partscan "$OUTPUT_DIR/disk.img")
echo "Loop device: $LOOP"

PART="${LOOP}p2"
i=0
while [ ! -e "$PART" ] && [ "$i" -lt 20 ]; do
    sleep 0.25
    i=$((i + 1))
done

if [ ! -e "$PART" ]; then
    echo "ERROR: Partition $PART did not appear"
    exit 1
fi

echo "Formatting ext4..."
mkfs.ext4 -q -L artix-root "$PART"

MNT="$OUTPUT_DIR/mnt"
mkdir -p "$MNT"
mount "$PART" "$MNT"

# ============================================================
# Part 2: Populate rootfs
# ============================================================
ARTIX_ROOTFS_CACHE="$CACHE_DIR/artix-sway-rootfs.tar.zst"

if [ -f "$ARTIX_ROOTFS_CACHE" ]; then
    echo "Using cached Artix+sway rootfs ($ARTIX_ROOTFS_CACHE)..."
    tar --zstd -xf "$ARTIX_ROOTFS_CACHE" -C "$MNT"
else
    echo "Building Artix rootfs via pacstrap (first run -- will be cached)..."

    # Write a temporary pacman.conf pointing at Artix repos.
    # SigLevel=Never is acceptable for ephemeral test images.
    PACMAN_CONF="$OUTPUT_DIR/pacman-artix.conf"
    cat > "$PACMAN_CONF" <<'PACCONF'
[options]
Architecture = x86_64
SigLevel = Never
ParallelDownloads = 5

[system]
Server = https://mirror1.artixlinux.org/repos/$repo/os/$arch

[world]
Server = https://mirror1.artixlinux.org/repos/$repo/os/$arch

[galaxy]
Server = https://mirror1.artixlinux.org/repos/$repo/os/$arch

# Arch extra packages forwarded by Artix mirror
[extra]
Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch
PACCONF

    pacstrap -C "$PACMAN_CONF" -K "$MNT" \
        base \
        linux-firmware \
        udev \
        dbus \
        kmod \
        iproute2 \
        util-linux \
        sway swaybg \
        seatd \
        foot \
        mesa mesa-utils \
        libinput \
        xkeyboard-config \
        polkit \
        || {
        echo "ERROR: pacstrap failed."
        exit 1
    }

    rm -f "$PACMAN_CONF"

    # pacstrap bind-mounts /proc, /dev, /sys into the target; clean up before
    # caching so the tarball doesn't capture stale mount points.
    umount "$MNT/proc" 2>/dev/null || true
    umount "$MNT/dev"  2>/dev/null || true
    umount "$MNT/sys"  2>/dev/null || true

    echo "Caching Artix rootfs for future runs..."
    tar --zstd -cf "$ARTIX_ROOTFS_CACHE" -C "$MNT" .
    echo "Cached at $ARTIX_ROOTFS_CACHE ($(du -sh "$ARTIX_ROOTFS_CACHE" | cut -f1))"
fi

# ============================================================
# Part 3: Install dynamod binaries (always fresh)
# ============================================================
echo "Installing dynamod binaries..."
install -Dm755 "$ZIG_OUT/dynamod-init"         "$MNT/sbin/dynamod-init"
install -Dm755 "$CARGO_OUT/dynamod-svmgr"      "$MNT/usr/lib/dynamod/dynamod-svmgr"
install -Dm755 "$CARGO_OUT/dynamodctl"          "$MNT/usr/bin/dynamodctl"
install -Dm755 "$CARGO_OUT/dynamod-logd"        "$MNT/usr/lib/dynamod/dynamod-logd"
install -Dm755 "$CARGO_OUT/dynamod-logind"      "$MNT/usr/lib/dynamod/dynamod-logind"
install -Dm755 "$CARGO_OUT/dynamod-sd1bridge"   "$MNT/usr/lib/dynamod/dynamod-sd1bridge"
install -Dm755 "$CARGO_OUT/dynamod-hostnamed"   "$MNT/usr/lib/dynamod/dynamod-hostnamed"

# ============================================================
# Part 4: Install service configs
# ============================================================
echo "Installing service configs..."
mkdir -p "$MNT/etc/dynamod/services" "$MNT/etc/dynamod/supervisors"

cp "$PROJECT_ROOT/config/supervisors/"*.toml "$MNT/etc/dynamod/supervisors/"

# Core services (skip mdev-coldplug -- we use udev on Artix).
# Skip syslog (Artix doesn't ship busybox syslogd).
for svc in fsck remount-root-rw machine-id fstab-mount modules-load \
           bootmisc hostname network sysctl dynamod-logd \
           dbus dynamod-logind dynamod-sd1bridge dynamod-hostnamed \
           udev udev-coldplug; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" "$MNT/etc/dynamod/services/"
    fi
done

# Artix doesn't ship inetutils, so /bin/hostname is missing.
# Override the hostname service to write via /proc instead.
cat > "$MNT/etc/dynamod/services/hostname.toml" <<'HOSTNAME_SVC'
[service]
name = "hostname"
supervisor = "early-boot"
exec = ["/bin/sh", "-c", "read -r h </etc/hostname 2>/dev/null && printf '%s' \"$h\" > /proc/sys/kernel/hostname"]
type = "oneshot"

[restart]
policy = "temporary"

[dependencies]
after = ["fstab-mount"]

[readiness]
type = "none"

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "3s"
HOSTNAME_SVC

# D-Bus policy files
mkdir -p "$MNT/usr/share/dbus-1/system.d"
cp "$PROJECT_ROOT/config/dbus-1/"*.conf "$MNT/usr/share/dbus-1/system.d/"

# Permissive D-Bus system.conf for testing
mkdir -p "$MNT/etc/dbus-1"
cat > "$MNT/etc/dbus-1/system.conf" <<'DBUSCONF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path=/run/dbus/system_bus_socket</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
    <allow send_type="method_call"/>
    <allow send_type="signal"/>
  </policy>
  <includedir>system.d</includedir>
  <includedir>/usr/share/dbus-1/system.d</includedir>
</busconfig>
DBUSCONF

# Override dbus service to create /run/dbus first (tmpfs is empty at boot)
cat > "$MNT/etc/dynamod/services/dbus.toml" <<'DBUS'
[service]
name = "dbus"
supervisor = "root"
exec = ["/bin/sh", "-c", "mkdir -p /run/dbus && exec /usr/bin/dbus-daemon --system --nofork --nopidfile"]
type = "simple"

[restart]
policy = "permanent"
delay = "1s"
max-restarts = 10
max-restart-window = "60s"

[readiness]
type = "none"
timeout = "10s"

[dependencies]
requires = ["bootmisc", "machine-id"]
after = ["bootmisc", "machine-id"]

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "5s"
DBUS

# Serial console getty
cat > "$MNT/etc/dynamod/services/getty-ttyS0.toml" <<'GETTY'
[service]
name = "getty-ttyS0"
exec = ["/usr/bin/agetty", "ttyS0", "115200", "vt100"]
type = "simple"

[restart]
policy = "permanent"
delay = "1s"
max-restarts = 10
max-restart-window = "30s"

[dependencies]
after = ["bootmisc"]

[readiness]
type = "none"

[shutdown]
stop-signal = "SIGHUP"
stop-timeout = "3s"
GETTY

# seatd service
cat > "$MNT/etc/dynamod/services/seatd.toml" <<'SEATD'
[service]
name = "seatd"
supervisor = "root"
exec = ["/usr/bin/seatd", "-g", "video"]
type = "simple"

[restart]
policy = "permanent"
delay = "1s"
max-restarts = 5
max-restart-window = "60s"

[dependencies]
requires = ["udev-coldplug"]

[readiness]
type = "none"

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "5s"
SEATD

# sway session service
cat > "$MNT/etc/dynamod/services/sway-session.toml" <<'SWAY'
[service]
name = "sway-session"
exec = ["/opt/sway-session.sh"]
type = "simple"

[restart]
policy = "permanent"
delay = "3s"
max-restarts = 3
max-restart-window = "60s"

[readiness]
type = "none"

[dependencies]
after = ["seatd", "dbus", "udev-coldplug"]
SWAY

# sway session startup script
cat > "$MNT/opt/sway-session.sh" <<'SWAYSESSION'
#!/bin/sh
LOG=/tmp/sway-session.log

log() {
    echo "$@"
    echo "$@" >> "$LOG"
    echo "$@" > /dev/ttyS0 2>/dev/null || true
}

: > "$LOG"
log "=== dynamod Artix sway session startup ==="

# Wait for dynamod
i=0
while [ "$i" -lt 30 ]; do
    [ -d /run/dynamod ] && break
    i=$((i + 1))
    sleep 1
done
if [ ! -d /run/dynamod ]; then
    log "FATAL: /run/dynamod not found"
    exit 1
fi

mkdir -p /run/dbus /tmp/.X11-unix
mkdir -p /run/user/0
chmod 0700 /run/user/0
rm -rf /var/run 2>/dev/null
ln -sf /run /var/run

# Wait for D-Bus socket
i=0
while [ "$i" -lt 30 ]; do
    [ -S /run/dbus/system_bus_socket ] && break
    i=$((i + 1))
    sleep 1
done
if [ ! -S /run/dbus/system_bus_socket ]; then
    log "WARNING: D-Bus system bus not found, continuing anyway"
fi

# Wait for seatd
i=0
while [ "$i" -lt 15 ]; do
    pgrep -x seatd >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done

# Write systemd-compatible session tracking files for libelogind compat
mkdir -p /run/systemd/sessions /run/systemd/seats /run/systemd/users
cat > /run/systemd/sessions/1 <<SESSIONFILE
UID=0
USER=root
ACTIVE=1
IS_DISPLAY=1
STATE=active
REMOTE=0
VTNR=1
SEAT=seat0
TYPE=wayland
CLASS=user
DESKTOP=sway
LEADER=$$
SESSIONFILE

cat > /run/systemd/seats/seat0 <<SEATFILE
IS_SEAT0=1
CAN_GRAPHICAL=1
CAN_TTY=1
ACTIVE_SESSION=1
SESSIONS=1
SEATFILE

cat > /run/systemd/users/0 <<USERFILE
NAME=root
STATE=active
SESSIONS=1
DISPLAY=1
USERFILE

# Environment for sway
export XDG_RUNTIME_DIR=/tmp/sway-run
mkdir -p "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"
rm -f "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null

export XDG_SESSION_TYPE=wayland
export XDG_SEAT=seat0
export XDG_SESSION_ID=1
export XDG_VTNR=1
unset WAYLAND_DISPLAY 2>/dev/null || true
export WLR_BACKENDS=drm
export WLR_RENDERER=pixman
export LIBSEAT_BACKEND=seatd

export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile >> "$LOG" 2>&1 &
sleep 1

# Start udevd for GPU enumeration if available and not already running
if ! pgrep -x udevd >/dev/null 2>&1; then
    if command -v udevd >/dev/null 2>&1; then
        udevd --daemon >> "$LOG" 2>&1
        udevadm trigger --action=add >> "$LOG" 2>&1
        udevadm settle --timeout=5 >> "$LOG" 2>&1
    fi
fi

# sway config
mkdir -p /root/.config/sway
cat > /root/.config/sway/config <<'SWAYCONF'
set $term foot
exec $term
bindsym Mod4+Shift+e exit
output * bg #285577 solid_color
bar {
    status_command echo "dynamod + sway on Artix (eudev + seatd)"
    position top
}
SWAYCONF

log "Launching sway..."
sway -d >> "$LOG" 2>&1
SWAY_EXIT=$?
log "sway exited: $SWAY_EXIT"

cat "$LOG" > /dev/ttyS0 2>/dev/null || true
exit $SWAY_EXIT
SWAYSESSION
chmod +x "$MNT/opt/sway-session.sh"

# In-VM verification script
install -Dm755 "$SCRIPT_DIR/artix-verify.sh" "$MNT/opt/artix-verify.sh"

cat > "$MNT/etc/dynamod/services/artix-verify.toml" <<'EOF'
[service]
name = "artix-verify"
exec = ["/opt/artix-verify.sh"]
type = "oneshot"

[restart]
policy = "temporary"

[readiness]
type = "none"

[dependencies]
after = ["bootmisc", "udev-coldplug"]
EOF

# ============================================================
# Part 5: System configuration
# ============================================================
echo "Configuring system..."

echo "dynamod-artix-test" > "$MNT/etc/hostname"
echo "127.0.0.1 localhost dynamod-artix-test" > "$MNT/etc/hosts"

# Empty root password for test
sed -i 's|^root:.*|root::0:0:root:/root:/bin/bash|' "$MNT/etc/passwd"

cat > "$MNT/etc/fstab" <<'FSTAB'
# dynamod Artix test fstab
/dev/vda2    /    ext4    defaults    0 1
FSTAB

rm -rf "$MNT/var/run" 2>/dev/null
ln -sf /run "$MNT/var/run"

touch "$MNT/etc/modules" "$MNT/etc/sysctl.conf"
mkdir -p "$MNT/var/log/dynamod" "$MNT/var/lib/dynamod" \
         "$MNT/run" "$MNT/run/dbus" "$MNT/tmp" \
         "$MNT/proc" "$MNT/sys" "$MNT/dev"
chmod 1777 "$MNT/tmp"

# ============================================================
# Part 6: Build minimal initramfs
# ============================================================
echo "Building initramfs..."
INITRAMFS_DIR="$OUTPUT_DIR/initramfs"
mkdir -p "$INITRAMFS_DIR"/{sbin,bin,dev,proc,sys,newroot}

cp "$ZIG_OUT/dynamod-init" "$INITRAMFS_DIR/sbin/dynamod-init"

# Use busybox from rootfs (Artix base includes busybox) or host
BUSYBOX=""
for bb in "$MNT/usr/bin/busybox" "$MNT/bin/busybox" \
          "$PROJECT_ROOT/test/qemu/busybox" \
          "$(command -v busybox 2>/dev/null)"; do
    [ -f "$bb" ] && BUSYBOX="$bb" && break
done

if [ -n "$BUSYBOX" ] && [ -f "$BUSYBOX" ]; then
    cp "$BUSYBOX" "$INITRAMFS_DIR/bin/busybox"
    for cmd in sh mdev mount umount losetup modprobe blkid; do
        ln -sf busybox "$INITRAMFS_DIR/bin/$cmd"
    done
    ln -sf ../bin/mdev "$INITRAMFS_DIR/sbin/mdev"
    mkdir -p "$INITRAMFS_DIR/etc"
    cat > "$INITRAMFS_DIR/etc/modules" <<'MODS'
scsi_mod
ata_piix
cdrom
sr_mod
squashfs
loop
iso9660
udf
overlay
MODS
    echo "  Included busybox for mdev, mount helpers"
else
    echo "  WARNING: No busybox found -- mdev won't be available in initramfs"
fi

cd "$INITRAMFS_DIR"
find . -print0 | cpio --null -ov --format=newc 2>/dev/null | gzip -9 > "$OUTPUT_DIR/initramfs.gz"
cd "$PROJECT_ROOT"

# Install kernel and initramfs into the disk image
echo "Installing kernel and initramfs to /boot/..."
mkdir -p "$MNT/boot"
cp "$KERNEL" "$MNT/boot/vmlinuz"
cp "$OUTPUT_DIR/initramfs.gz" "$MNT/boot/initramfs-dynamod.gz"

# ============================================================
# Part 7: Install GRUB bootloader
# ============================================================
echo "Installing GRUB bootloader..."

"$GRUB_INSTALL" --target=i386-pc --boot-directory="$MNT/boot" "$LOOP"

if [ -d "$MNT/boot/grub2" ]; then
    GRUB_CFG_PATH="$MNT/boot/grub2/grub.cfg"
elif [ -d "$MNT/boot/grub" ]; then
    GRUB_CFG_PATH="$MNT/boot/grub/grub.cfg"
else
    echo "ERROR: Neither boot/grub nor boot/grub2 after grub-install."
    exit 1
fi
mkdir -p "$(dirname "$GRUB_CFG_PATH")"

cat > "$GRUB_CFG_PATH" <<'GRUBCFG'
set timeout=0
set default=0

menuentry "Artix Linux (dynamod)" {
    linux /boot/vmlinuz console=ttyS0 earlyprintk=ttyS0 root=/dev/vda2 rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init panic=10
    initrd /boot/initramfs-dynamod.gz
}
GRUBCFG

echo "GRUB installed successfully."

# ============================================================
# Cleanup
# ============================================================
echo "Unmounting..."
umount "$MNT/proc" 2>/dev/null || true
umount "$MNT/dev"  2>/dev/null || true
umount "$MNT/sys"  2>/dev/null || true
umount "$MNT"
MNT=""
losetup -d "$LOOP"
LOOP=""

echo ""
echo "=== Build Complete ==="
echo "Disk image:  $OUTPUT_DIR/disk.img (${DISK_SIZE_MB}MB)"
echo "Initramfs:   $OUTPUT_DIR/initramfs.gz ($(du -sh "$OUTPUT_DIR/initramfs.gz" | cut -f1))"
echo ""
echo "Boot with:"
echo "  qemu-system-x86_64 \\"
echo "    -drive file=$OUTPUT_DIR/disk.img,format=raw,if=virtio \\"
echo "    -nographic -no-reboot -m 2048M"
