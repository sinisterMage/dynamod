#!/bin/sh
# Build and boot-test dynamod as PID 1 on an Alpine Linux rootfs in QEMU.
#
# Prerequisites:
#   - qemu-system-x86_64
#   - wget (or curl)
#   - A Linux kernel bzImage (build or provide via KERNEL= env var)
#
# Usage:
#   make test-alpine
#   # or directly:
#   KERNEL=/path/to/bzImage test/alpine/build-test.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
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

QEMU_TIMEOUT="${QEMU_TIMEOUT:-60}"

# Find kernel
if [ -z "$KERNEL" ]; then
    # Try common locations
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

echo "=== dynamod Alpine Integration Test ==="
echo "Kernel:  $KERNEL"
echo "Alpine:  $ALPINE_RELEASE"
echo "Timeout: ${QEMU_TIMEOUT}s"
echo ""

# Check binaries exist
for bin in "$ZIG_OUT/dynamod-init" \
           "$CARGO_OUT/dynamod-svmgr" \
           "$CARGO_OUT/dynamodctl" \
           "$CARGO_OUT/dynamod-logd"; do
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found. Run 'make' first."
        exit 1
    fi
done

# Clean and create build directory
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/rootfs"

# Download Alpine minirootfs if not cached
CACHE_DIR="$SCRIPT_DIR/.cache"
mkdir -p "$CACHE_DIR"
if [ ! -f "$CACHE_DIR/$ALPINE_ROOTFS" ]; then
    echo "Downloading Alpine minirootfs..."
    wget -q -O "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL" || \
    curl -sL -o "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL"
fi

# Extract rootfs
echo "Extracting Alpine rootfs..."
cd "$BUILD_DIR/rootfs"
tar xzf "$CACHE_DIR/$ALPINE_ROOTFS"

# Install dynamod binaries
echo "Installing dynamod..."
install -Dm755 "$ZIG_OUT/dynamod-init"      sbin/dynamod-init
install -Dm755 "$CARGO_OUT/dynamod-svmgr"   usr/lib/dynamod/dynamod-svmgr
install -Dm755 "$CARGO_OUT/dynamodctl"       usr/bin/dynamodctl
install -Dm755 "$CARGO_OUT/dynamod-logd"     usr/lib/dynamod/dynamod-logd

# Install service configs
mkdir -p etc/dynamod/services etc/dynamod/supervisors
cp "$PROJECT_ROOT/config/supervisors/"*.toml etc/dynamod/supervisors/

# Install only Alpine-relevant services (skip agetty-tty1, we use serial getty)
for svc in fstab-mount modules-load mdev-coldplug bootmisc hostname \
           network sysctl syslog dynamod-logd; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" etc/dynamod/services/
    fi
done

# Create a serial console getty for QEMU testing
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

# Set up Alpine basics
echo "dynamod-test" > etc/hostname
echo "127.0.0.1 localhost dynamod-test" > etc/hosts

# Set root password to empty for test login
sed -i 's|^root:.*|root::0:0:root:/root:/bin/sh|' etc/passwd

# Create busybox symlinks (minirootfs may not have all of them)
for cmd in getty agetty mount umount modprobe sysctl mdev ip hostname \
           syslogd sh cat echo mkdir chmod rm true false kill ps sleep; do
    [ ! -e "sbin/$cmd" ] && [ ! -e "bin/$cmd" ] && \
        ln -sf /bin/busybox "sbin/$cmd" 2>/dev/null || true
done

# Create /etc/modules (empty is fine)
touch etc/modules

# Create fstab (minimal for minirootfs — no real partitions to mount)
cat > etc/fstab <<'FSTAB'
# dynamod test fstab
# No real disk partitions in QEMU initramfs mode
FSTAB

# Ensure sysctl.conf exists
touch etc/sysctl.conf

# Create required runtime directories
mkdir -p var/log var/lib/dynamod run tmp
chmod 1777 tmp

# Build initramfs (cpio + gzip)
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"

cd "$PROJECT_ROOT"

echo "Booting QEMU (timeout: ${QEMU_TIMEOUT}s)..."
echo "---"

# Boot QEMU
timeout --foreground "$QEMU_TIMEOUT" \
    qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=ttyS0 earlyprintk=ttyS0 rdinit=/sbin/dynamod-init panic=5" \
    -nographic \
    -no-reboot \
    -m 256M \
    -smp 1 \
    || true

echo ""
echo "---"
echo "QEMU exited. Review output above for boot success."
echo "Look for: 'dynamod-svmgr starting' and service startup messages."

# Cleanup
rm -rf "$BUILD_DIR"
echo "Done."
