#!/bin/sh
# Boot dynamod on Alpine Linux in a QEMU graphical window.
#
# Usage:
#   ./test/alpine/boot-gui.sh
#   KERNEL=/path/to/bzImage ./test/alpine/boot-gui.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-gui"

ZIG_OUT="$PROJECT_ROOT/zig/zig-out/bin"
CARGO_OUT="$(if [ -d "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release" ]; then
    echo "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release"
else
    echo "$PROJECT_ROOT/rust/target/release"
fi)"

ALPINE_VERSION="${ALPINE_VERSION:-3.21}"
ALPINE_RELEASE="${ALPINE_RELEASE:-3.21.3}"
ALPINE_ROOTFS="alpine-minirootfs-${ALPINE_RELEASE}-x86_64.tar.gz"
ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VERSION}/releases/x86_64/${ALPINE_ROOTFS}"

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

# Check binaries
for bin in "$ZIG_OUT/dynamod-init" "$CARGO_OUT/dynamod-svmgr" \
           "$CARGO_OUT/dynamodctl" "$CARGO_OUT/dynamod-logd"; do
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found. Run 'make' first."
        exit 1
    fi
done

# Download Alpine minirootfs if not cached
CACHE_DIR="$SCRIPT_DIR/.cache"
mkdir -p "$CACHE_DIR"
if [ ! -f "$CACHE_DIR/$ALPINE_ROOTFS" ]; then
    echo "Downloading Alpine minirootfs..."
    wget -q -O "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL" || \
    curl -sL -o "$CACHE_DIR/$ALPINE_ROOTFS" "$ALPINE_URL"
fi

# Build rootfs
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/rootfs"
cd "$BUILD_DIR/rootfs"
tar xzf "$CACHE_DIR/$ALPINE_ROOTFS"

# Install dynamod
install -Dm755 "$ZIG_OUT/dynamod-init"      sbin/dynamod-init
install -Dm755 "$CARGO_OUT/dynamod-svmgr"   usr/lib/dynamod/dynamod-svmgr
install -Dm755 "$CARGO_OUT/dynamodctl"       usr/bin/dynamodctl
install -Dm755 "$CARGO_OUT/dynamod-logd"     usr/lib/dynamod/dynamod-logd

# Install configs
mkdir -p etc/dynamod/services etc/dynamod/supervisors
cp "$PROJECT_ROOT/config/supervisors/"*.toml etc/dynamod/supervisors/

for svc in fstab-mount modules-load mdev-coldplug bootmisc hostname \
           network sysctl syslog dynamod-logd; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" etc/dynamod/services/
    fi
done

# Getty on tty1 for the graphical window
cat > etc/dynamod/services/getty-tty1.toml <<'GETTY'
[service]
name = "getty-tty1"
exec = ["/sbin/getty", "tty1", "38400", "linux"]
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
echo "dynamod-test" > etc/hostname
echo "127.0.0.1 localhost dynamod-test" > etc/hosts
sed -i 's|^root:.*|root::0:0:root:/root:/bin/sh|' etc/passwd

# Busybox symlinks
for cmd in getty agetty mount umount modprobe sysctl mdev ip hostname \
           syslogd sh cat echo mkdir chmod rm true false kill ps sleep; do
    [ ! -e "sbin/$cmd" ] && [ ! -e "bin/$cmd" ] && \
        ln -sf /bin/busybox "sbin/$cmd" 2>/dev/null || true
done

touch etc/modules etc/sysctl.conf
mkdir -p var/log var/lib/dynamod run tmp
chmod 1777 tmp

# Build initramfs
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"
cd "$PROJECT_ROOT"

echo "Launching QEMU window..."
qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=tty0 rdinit=/sbin/dynamod-init panic=5" \
    -m 256M \
    -smp 1 \
    -display gtk \
    -vga std \
    -no-reboot &

QEMU_PID=$!
echo "QEMU running (PID $QEMU_PID). Close the window to stop."
wait $QEMU_PID 2>/dev/null || true

rm -rf "$BUILD_DIR"
echo "Done."
