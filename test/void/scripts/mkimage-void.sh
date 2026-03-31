#!/bin/sh
# Create a bootable GPT disk image with GRUB + Void Linux + dynamod.
#
# Produces a complete bootable disk image that QEMU can boot from cold
# (SeaBIOS -> GRUB -> kernel -> initramfs -> switch_root -> full userspace).
#
# Usage:
#   sudo test/void/scripts/mkimage-void.sh [output-dir]
#
# Environment variables:
#   KERNEL       - Path to bzImage (auto-detected if unset)
#   DISK_SIZE_MB - Disk image size in MB (default: 2048)
#   VOID_DATE    - Void rootfs tarball date stamp (default: 20250219)
#
# Prerequisites:
#   - sfdisk, mkfs.ext4, losetup, mount (util-linux)
#   - grub-install (grub / grub2 / grub-pc-bin depending on distro)
#   - cpio, gzip
#   - wget or curl

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
OUTPUT_DIR="${1:-$TEST_DIR/build-full}"
DISK_SIZE_MB="${DISK_SIZE_MB:-2048}"

VOID_DATE="${VOID_DATE:-20250202}"
VOID_ARCH="x86_64"
VOID_LIBC="musl"
VOID_MIRROR="https://repo-default.voidlinux.org/live/current"
VOID_ROOTFS="void-${VOID_ARCH}-${VOID_LIBC}-ROOTFS-${VOID_DATE}.tar.xz"
VOID_URL="${VOID_MIRROR}/${VOID_ROOTFS}"

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
echo "=== dynamod Void Linux Disk Image Builder ==="
echo "Output: $OUTPUT_DIR"
echo "Disk:   ${DISK_SIZE_MB}MB"
echo ""

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script requires root (for losetup, mount, chroot, grub-install)."
    echo "  Run: sudo $0 $*"
    exit 1
fi

# Check for grub-install (different names on different distros)
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
    echo "  Install one of: grub (Void/Arch), grub2 (Fedora), grub-pc-bin (Debian/Ubuntu)"
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
# Download Void rootfs
# ============================================================
mkdir -p "$CACHE_DIR"
if [ ! -f "$CACHE_DIR/$VOID_ROOTFS" ]; then
    echo "Downloading Void Linux rootfs ($VOID_ROOTFS)..."
    echo "  URL: $VOID_URL"
    if command -v wget >/dev/null 2>&1; then
        wget -q --show-progress -O "$CACHE_DIR/$VOID_ROOTFS.part" "$VOID_URL" || {
            rm -f "$CACHE_DIR/$VOID_ROOTFS.part"
            echo "ERROR: Download failed. The rootfs date '$VOID_DATE' may no longer exist on the mirror."
            echo "  Check https://repo-default.voidlinux.org/live/current/ for available dates"
            echo "  and re-run with: VOID_DATE=<date> $0"
            exit 1
        }
    elif command -v curl >/dev/null 2>&1; then
        curl -fL --progress-bar -o "$CACHE_DIR/$VOID_ROOTFS.part" "$VOID_URL" || {
            rm -f "$CACHE_DIR/$VOID_ROOTFS.part"
            echo "ERROR: Download failed. The rootfs date '$VOID_DATE' may no longer exist on the mirror."
            echo "  Check https://repo-default.voidlinux.org/live/current/ for available dates"
            echo "  and re-run with: VOID_DATE=<date> $0"
            exit 1
        }
    else
        echo "ERROR: wget or curl required"
        exit 1
    fi
    mv "$CACHE_DIR/$VOID_ROOTFS.part" "$CACHE_DIR/$VOID_ROOTFS"
    echo "Downloaded to $CACHE_DIR/$VOID_ROOTFS"
fi

# ============================================================
# Prepare output directory
# ============================================================
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# ============================================================
# Part 1: Create GPT disk image
# ============================================================
echo "Creating ${DISK_SIZE_MB}MB disk image..."
dd if=/dev/zero of="$OUTPUT_DIR/disk.img" bs=1M count="$DISK_SIZE_MB" status=none

# GPT: 2MB BIOS boot partition + rest as Linux root
echo "Partitioning (GPT + BIOS boot)..."
sfdisk "$OUTPUT_DIR/disk.img" --quiet <<EOF
label: gpt
size=2M, type=21686148-6449-6E6F-744E-656564454649
type=linux
EOF

# Set up loop device
LOOP=$(losetup --find --show --partscan "$OUTPUT_DIR/disk.img")
echo "Loop device: $LOOP"

# Wait for partition devices to appear
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

# Format root partition
echo "Formatting ext4..."
mkfs.ext4 -q -L void-root "$PART"

# Mount
MNT="$OUTPUT_DIR/mnt"
mkdir -p "$MNT"
mount "$PART" "$MNT"

# ============================================================
# Part 2: Populate rootfs
# ============================================================
DBUS_ROOTFS_CACHE="$CACHE_DIR/void-dbus-rootfs-${VOID_DATE}.tar.xz"

if [ -f "$DBUS_ROOTFS_CACHE" ]; then
    echo "Using cached Void+D-Bus rootfs ($DBUS_ROOTFS_CACHE)..."
    tar xJf "$DBUS_ROOTFS_CACHE" -C "$MNT"
else
    echo "Building Void+D-Bus rootfs (first run -- will be cached for next time)..."
    tar xJf "$CACHE_DIR/$VOID_ROOTFS" -C "$MNT"

    # Install dbus via chroot
    cp /etc/resolv.conf "$MNT/etc/resolv.conf" 2>/dev/null || \
        echo "nameserver 8.8.8.8" > "$MNT/etc/resolv.conf"

    mount --bind /proc "$MNT/proc"
    mount --bind /dev  "$MNT/dev"
    mount --bind /sys  "$MNT/sys"

    # Void requires xbps to be updated before installing packages
    echo "Updating xbps package manager..."
    chroot "$MNT" xbps-install -Syu xbps -y || {
        echo "ERROR: Failed to update xbps inside Void chroot."
        exit 1
    }

    # Install packages needed for dynamod services:
    #   dbus        - D-Bus system bus (required for mimic services)
    #   busybox     - syslogd, mdev, hostname, and other tools used by service configs
    echo "Installing dbus and busybox..."
    chroot "$MNT" xbps-install -Sy dbus busybox || {
        echo "ERROR: Failed to install packages inside Void chroot."
        exit 1
    }

    umount "$MNT/proc"
    umount "$MNT/dev"
    umount "$MNT/sys"

    # Cache for future runs
    echo "Caching Void+D-Bus rootfs..."
    tar cJf "$DBUS_ROOTFS_CACHE" -C "$MNT" .
    echo "Cached at $DBUS_ROOTFS_CACHE ($(du -sh "$DBUS_ROOTFS_CACHE" | cut -f1))"
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

# All supervisor configs
cp "$PROJECT_ROOT/config/supervisors/"*.toml "$MNT/etc/dynamod/supervisors/"

# Only install services whose binaries exist in the Void rootfs.
# Skip: syslog (no syslogd in Void), mdev-coldplug (no mdev, devtmpfs is fine),
#        example/demo services (nginx, postgresql, sshd, example).
for svc in fsck remount-root-rw fstab-mount modules-load \
           bootmisc hostname network sysctl dynamod-logd \
           dbus dynamod-logind dynamod-sd1bridge dynamod-hostnamed; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" "$MNT/etc/dynamod/services/"
    fi
done

# D-Bus policy files
mkdir -p "$MNT/usr/share/dbus-1/system.d"
cp "$PROJECT_ROOT/config/dbus-1/"*.conf "$MNT/usr/share/dbus-1/system.d/"

# D-Bus system bus configuration (permissive for testing)
# Always overwrite -- Void's dbus package installs a restrictive default
# that prevents our mimic daemons from registering names.
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
after = ["bootmisc"]

[shutdown]
stop-signal = "SIGTERM"
stop-timeout = "5s"
DBUS

# Serial console getty (Void uses agetty from util-linux, not busybox getty)
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

# Install in-VM verification script as a dynamod oneshot service
install -Dm755 "$SCRIPT_DIR/void-verify.sh" "$MNT/opt/void-verify.sh"

cat > "$MNT/etc/dynamod/services/void-verify.toml" <<'EOF'
[service]
name = "void-verify"
exec = ["/opt/void-verify.sh"]
type = "oneshot"

[restart]
policy = "temporary"

[readiness]
type = "none"

[dependencies]
after = ["bootmisc"]
EOF

# ============================================================
# Part 5: System configuration
# ============================================================
echo "Configuring system..."

echo "dynamod-void-test" > "$MNT/etc/hostname"
echo "127.0.0.1 localhost dynamod-void-test" > "$MNT/etc/hosts"

# Empty root password for test
sed -i 's|^root:.*|root::0:0:root:/root:/bin/sh|' "$MNT/etc/passwd"

# fstab
cat > "$MNT/etc/fstab" <<'FSTAB'
# dynamod Void test fstab
/dev/vda2    /    ext4    defaults    0 1
FSTAB

# Ensure /var/run -> /run symlink (critical for libelogind and D-Bus)
rm -rf "$MNT/var/run" 2>/dev/null
ln -sf /run "$MNT/var/run"

# Create required files and directories
touch "$MNT/etc/modules" "$MNT/etc/sysctl.conf"
mkdir -p "$MNT/var/log/dynamod" "$MNT/var/lib/dynamod" \
         "$MNT/run" "$MNT/run/dbus" "$MNT/tmp" \
         "$MNT/proc" "$MNT/sys" "$MNT/dev"
chmod 1777 "$MNT/tmp"

# Busybox symlinks for commands that services need
# Void musl rootfs has busybox at /usr/bin/busybox
if [ -f "$MNT/usr/bin/busybox" ]; then
    BUSYBOX_PATH="/usr/bin/busybox"
elif [ -f "$MNT/bin/busybox" ]; then
    BUSYBOX_PATH="/bin/busybox"
else
    BUSYBOX_PATH=""
fi

if [ -n "$BUSYBOX_PATH" ]; then
    mkdir -p "$MNT/sbin" "$MNT/bin"
    # Create symlinks in /sbin for system commands
    for cmd in mount umount modprobe sysctl mdev ip syslogd fsck; do
        [ ! -e "$MNT/sbin/$cmd" ] && \
            ln -sf "$BUSYBOX_PATH" "$MNT/sbin/$cmd" 2>/dev/null || true
    done
    # Create symlinks in /bin for user commands
    for cmd in hostname sh cat echo mkdir chmod rm true false \
               kill ps sleep grep wc seq dbus-uuidgen pgrep; do
        [ ! -e "$MNT/bin/$cmd" ] && [ ! -e "$MNT/usr/bin/$cmd" ] && \
            ln -sf "$BUSYBOX_PATH" "$MNT/bin/$cmd" 2>/dev/null || true
    done
fi

# ============================================================
# Part 6: Build minimal initramfs
# ============================================================
echo "Building initramfs..."
INITRAMFS_DIR="$OUTPUT_DIR/initramfs"
mkdir -p "$INITRAMFS_DIR"/{sbin,bin,dev,proc,sys,newroot}

# Copy dynamod-init
cp "$ZIG_OUT/dynamod-init" "$INITRAMFS_DIR/sbin/dynamod-init"

# Copy busybox for mdev during initramfs phase
BUSYBOX=""
if [ -n "$BUSYBOX_PATH" ] && [ -f "$MNT$BUSYBOX_PATH" ]; then
    BUSYBOX="$MNT$BUSYBOX_PATH"
else
    # Fallback to host busybox
    for bb in "$PROJECT_ROOT/test/qemu/busybox" "$(command -v busybox 2>/dev/null)"; do
        [ -f "$bb" ] && BUSYBOX="$bb" && break
    done
fi

if [ -n "$BUSYBOX" ] && [ -f "$BUSYBOX" ]; then
    cp "$BUSYBOX" "$INITRAMFS_DIR/bin/busybox"
    for cmd in sh mdev mount umount; do
        ln -sf busybox "$INITRAMFS_DIR/bin/$cmd"
    done
    ln -sf ../bin/mdev "$INITRAMFS_DIR/sbin/mdev"
    echo "  Included busybox for mdev"
else
    echo "  WARNING: No busybox found -- mdev won't be available in initramfs"
fi

# Pack initramfs
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

# Install GRUB to the disk image (using host grub-install)
"$GRUB_INSTALL" --target=i386-pc --boot-directory="$MNT/boot" "$LOOP"

# grub2-install (Fedora/RHEL) uses boot/grub2/; Debian/Ubuntu grub-install uses boot/grub/.
# Config must live next to the modules or GRUB drops to the interactive shell with no menu.
if [ -d "$MNT/boot/grub2" ]; then
    GRUB_CFG_PATH="$MNT/boot/grub2/grub.cfg"
elif [ -d "$MNT/boot/grub" ]; then
    GRUB_CFG_PATH="$MNT/boot/grub/grub.cfg"
else
    echo "ERROR: Neither boot/grub nor boot/grub2 after grub-install."
    exit 1
fi
mkdir -p "$(dirname "$GRUB_CFG_PATH")"

# Create GRUB configuration
cat > "$GRUB_CFG_PATH" <<'GRUBCFG'
set timeout=0
set default=0

menuentry "Void Linux (dynamod)" {
    linux /boot/vmlinuz console=ttyS0 earlyprintk=ttyS0 root=/dev/vda2 rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init panic=10
    initrd /boot/initramfs-dynamod.gz
}
GRUBCFG

echo "GRUB installed successfully."

# ============================================================
# Cleanup
# ============================================================
echo "Unmounting..."
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
echo "    -nographic -no-reboot -m 1024M"
