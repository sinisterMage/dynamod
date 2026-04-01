#!/bin/sh
# Create a bootable disk image with Alpine + dynamod for QEMU testing.
#
# Produces:
#   - disk.img:      Raw disk image with ext4 root partition
#   - initramfs.gz:  Minimal initramfs (dynamod-init + busybox for mdev)
#
# The initramfs handles device detection and switch_root to the disk.
# The disk contains the full Alpine rootfs with dynamod binaries.
#
# Usage:
#   sudo tools/mkimage.sh [output-dir]
#
# Prerequisites:
#   - sfdisk, mkfs.ext4, losetup, mount (from util-linux)
#   - cpio, gzip
#   - wget or curl (for Alpine download)
#
# Custom / modular kernel (live ISO, /dev/sr0, squashfs as modules):
#   The default initramfs has no /lib/modules, so busybox modprobe cannot load drivers.
#   Either build those drivers built-in (=y), or bundle the matching tree:
#     INITRAMFS_MODULES_DIR=/lib/modules/$(uname -r) sudo -E tools/mkimage.sh
#   Use the same kernel version you pass to QEMU (-kernel).

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="${1:-$PROJECT_ROOT/test/alpine/build-disk}"
DISK_SIZE_MB="${DISK_SIZE_MB:-512}"

ALPINE_VERSION="${ALPINE_VERSION:-3.21}"
ALPINE_RELEASE="${ALPINE_RELEASE:-3.21.3}"
ALPINE_ARCH="x86_64"
ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"
ALPINE_ROOTFS="alpine-minirootfs-${ALPINE_RELEASE}-${ALPINE_ARCH}.tar.gz"
ALPINE_URL="${ALPINE_MIRROR}/v${ALPINE_VERSION}/releases/${ALPINE_ARCH}/${ALPINE_ROOTFS}"

ZIG_OUT="$PROJECT_ROOT/zig/zig-out/bin"
CARGO_OUT="$(if [ -d "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release" ]; then
    echo "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release"
else
    echo "$PROJECT_ROOT/rust/target/release"
fi)"

CACHE_DIR="$PROJECT_ROOT/test/alpine/.cache"

echo "=== dynamod Disk Image Builder ==="
echo "Output: $OUTPUT_DIR"
echo "Disk:   ${DISK_SIZE_MB}MB"
echo ""

# Check prerequisites
for cmd in sfdisk mkfs.ext4 losetup cpio gzip; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: $cmd not found"
        exit 1
    fi
done

# Check binaries
for bin in "$ZIG_OUT/dynamod-init" \
           "$CARGO_OUT/dynamod-svmgr" \
           "$CARGO_OUT/dynamodctl" \
           "$CARGO_OUT/dynamod-logd"; do
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found. Run 'make' first."
        exit 1
    fi
done

# Download Alpine if not cached
mkdir -p "$CACHE_DIR"
if [ ! -f "$CACHE_DIR/$ALPINE_ROOTFS" ]; then
    echo "Downloading Alpine minirootfs..."
    wget -q -O "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL" || \
    curl -sL -o "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL"
fi

# Clean and prepare output
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# ============================================================
# Part 1: Create the disk image with ext4 root filesystem
# ============================================================

echo "Creating ${DISK_SIZE_MB}MB disk image..."
dd if=/dev/zero of="$OUTPUT_DIR/disk.img" bs=1M count="$DISK_SIZE_MB" status=none

# Partition: single ext4 partition filling the whole disk
echo "Partitioning..."
sfdisk "$OUTPUT_DIR/disk.img" --quiet <<EOF
label: dos
type=83
EOF

# Set up loop device
LOOP=$(losetup --find --show --partscan "$OUTPUT_DIR/disk.img")
echo "Loop device: $LOOP"

# Wait for partition device to appear
PART="${LOOP}p1"
i=0
while [ ! -e "$PART" ] && [ "$i" -lt 20 ]; do
    sleep 0.25
    i=$((i + 1))
done

if [ ! -e "$PART" ]; then
    echo "ERROR: Partition $PART did not appear"
    losetup -d "$LOOP"
    exit 1
fi

# Format
echo "Formatting ext4..."
mkfs.ext4 -q -L dynamod-root "$PART"

# Mount and populate
MNT="$OUTPUT_DIR/mnt"
mkdir -p "$MNT"
mount "$PART" "$MNT"

echo "Installing Alpine rootfs..."
cd "$MNT"
tar xzf "$CACHE_DIR/$ALPINE_ROOTFS"

# Install dynamod binaries
echo "Installing dynamod..."
install -Dm755 "$ZIG_OUT/dynamod-init"      sbin/dynamod-init
install -Dm755 "$CARGO_OUT/dynamod-svmgr"   usr/lib/dynamod/dynamod-svmgr
install -Dm755 "$CARGO_OUT/dynamodctl"       usr/bin/dynamodctl
install -Dm755 "$CARGO_OUT/dynamod-logd"     usr/lib/dynamod/dynamod-logd

# Install mimic binaries if available
for bin in dynamod-logind dynamod-sd1bridge dynamod-hostnamed; do
    if [ -f "$CARGO_OUT/$bin" ]; then
        install -Dm755 "$CARGO_OUT/$bin" usr/lib/dynamod/$bin
    fi
done

# Install configs
mkdir -p etc/dynamod/services etc/dynamod/supervisors
cp "$PROJECT_ROOT/config/supervisors/"*.toml etc/dynamod/supervisors/

for svc in fstab-mount modules-load mdev-coldplug bootmisc hostname \
           network sysctl syslog dynamod-logd fsck remount-root-rw machine-id; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" etc/dynamod/services/
    fi
done

# Install D-Bus policy files if present
if [ -d "$PROJECT_ROOT/config/dbus-1" ]; then
    mkdir -p usr/share/dbus-1/system.d
    cp "$PROJECT_ROOT/config/dbus-1/"*.conf usr/share/dbus-1/system.d/ 2>/dev/null || true
fi

# Serial console getty
cat > etc/dynamod/services/getty-ttyS0.toml <<'GETTY'
[service]
name = "getty-ttyS0"
exec = ["/sbin/getty", "ttyS0", "115200", "vt100"]
type = "simple"

[restart]
policy = "permanent"
delay = "1s"
max-restarts = 10
max-restart-window = "30s"

[dependencies]
after = ["bootmisc", "mdev-coldplug"]

[readiness]
type = "none"

[shutdown]
stop-signal = "SIGHUP"
stop-timeout = "3s"
GETTY

# System setup
echo "dynamod-disk-test" > etc/hostname
echo "127.0.0.1 localhost dynamod-disk-test" > etc/hosts
sed -i 's|^root:.*|root::0:0:root:/root:/bin/sh|' etc/passwd

# Busybox symlinks
for cmd in getty agetty mount umount modprobe sysctl mdev ip hostname \
           syslogd sh cat echo mkdir chmod rm true false kill ps sleep \
           grep wc fsck; do
    [ ! -e "sbin/$cmd" ] && [ ! -e "bin/$cmd" ] && \
        ln -sf /bin/busybox "sbin/$cmd" 2>/dev/null || true
done

touch etc/modules etc/sysctl.conf
cat > etc/fstab <<'FSTAB'
# dynamod disk test fstab
# Root is mounted by initramfs, remounted rw by remount-root-rw service
FSTAB

mkdir -p var/log var/lib/dynamod run tmp proc sys dev
chmod 1777 tmp

cd "$PROJECT_ROOT"
umount "$MNT"
echo "Root filesystem populated."

# ============================================================
# Part 2: Create minimal initramfs
# ============================================================

echo "Building initramfs..."
INITRAMFS_DIR="$OUTPUT_DIR/initramfs"
mkdir -p "$INITRAMFS_DIR"/{sbin,bin,dev,proc,sys,newroot}

# Copy dynamod-init
cp "$ZIG_OUT/dynamod-init" "$INITRAMFS_DIR/sbin/dynamod-init"

# Copy busybox (for mdev -s during initramfs phase)
BUSYBOX="$PROJECT_ROOT/test/qemu/busybox"
if [ ! -f "$BUSYBOX" ]; then
    BUSYBOX=$(command -v busybox 2>/dev/null || true)
fi
if [ -n "$BUSYBOX" ] && [ -f "$BUSYBOX" ]; then
    cp "$BUSYBOX" "$INITRAMFS_DIR/bin/busybox"
    # Create symlinks needed during initramfs phase
    for cmd in sh mdev mount umount losetup modprobe blkid; do
        ln -sf busybox "$INITRAMFS_DIR/bin/$cmd"
    done
    ln -sf ../bin/mdev "$INITRAMFS_DIR/sbin/mdev"
    mkdir -p "$INITRAMFS_DIR/etc"
    # Hints for modular kernels (dynamod-init uses in-kernel loop ioctls; FS may still be modules)
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
    echo "  Included busybox for mdev, mount helpers, and etc/modules hints"
else
    echo "  WARNING: No busybox found — UUID/LABEL resolution won't work"
fi

if [ -n "${INITRAMFS_MODULES_DIR:-}" ] && [ -d "$INITRAMFS_MODULES_DIR" ]; then
    KVER="$(basename "$INITRAMFS_MODULES_DIR")"
    echo "  Bundling kernel modules: $INITRAMFS_MODULES_DIR -> lib/modules/$KVER"
    mkdir -p "$INITRAMFS_DIR/lib/modules"
    cp -a "$INITRAMFS_MODULES_DIR" "$INITRAMFS_DIR/lib/modules/$KVER"
else
    if [ -n "${INITRAMFS_MODULES_DIR:-}" ]; then
        echo "  WARNING: INITRAMFS_MODULES_DIR=$INITRAMFS_MODULES_DIR is not a directory"
    fi
fi

# Pack initramfs
cd "$INITRAMFS_DIR"
find . -print0 | cpio --null -ov --format=newc 2>/dev/null | gzip -9 > "$OUTPUT_DIR/initramfs.gz"

cd "$PROJECT_ROOT"

# Clean up loop device
losetup -d "$LOOP"

echo ""
echo "=== Build Complete ==="
echo "Disk image:  $OUTPUT_DIR/disk.img (${DISK_SIZE_MB}MB)"
echo "Initramfs:   $OUTPUT_DIR/initramfs.gz ($(du -sh "$OUTPUT_DIR/initramfs.gz" | cut -f1))"
echo ""
echo "Boot with:"
echo "  qemu-system-x86_64 -kernel <bzImage> \\"
echo "    -initrd $OUTPUT_DIR/initramfs.gz \\"
echo "    -drive file=$OUTPUT_DIR/disk.img,format=raw,if=virtio \\"
echo "    -append 'console=ttyS0 root=/dev/vda1 rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init' \\"
echo "    -nographic -no-reboot -m 512M"
