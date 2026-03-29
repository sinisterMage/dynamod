#!/bin/sh
# Boot dynamod with sway (Wayland compositor) in a QEMU graphical window.
#
# Tests that sway can start and acquire DRM/input devices via dynamod-logind's
# TakeDevice D-Bus method — the critical path for Wayland compatibility.
#
# Prerequisites:
#   - qemu-system-x86_64 (with GTK display support)
#   - wget or curl
#   - A Linux kernel bzImage with DRM/virtio-gpu support
#
# Usage:
#   ./test/alpine/boot-wayland.sh
#   KERNEL=/path/to/bzImage ./test/alpine/boot-wayland.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-wayland"
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
    echo "  The kernel needs DRM and virtio-gpu support."
    exit 1
fi

echo "=== dynamod Wayland (sway) Test ==="
echo "Kernel: $KERNEL"
echo "Alpine: $ALPINE_RELEASE"
echo ""

# Check binaries
for bin in "$ZIG_OUT/dynamod-init" \
           "$CARGO_OUT/dynamod-svmgr" \
           "$CARGO_OUT/dynamodctl" \
           "$CARGO_OUT/dynamod-logd" \
           "$CARGO_OUT/dynamod-logind" \
           "$CARGO_OUT/dynamod-sd1bridge" \
           "$CARGO_OUT/dynamod-hostnamed"; do
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found. Run 'make' or 'cargo build --release' first."
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

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/rootfs"

# Cache the rootfs with sway packages pre-installed to skip downloads on re-runs.
SWAY_ROOTFS_CACHE="$CACHE_DIR/alpine-sway-rootfs-${ALPINE_RELEASE}.tar.gz"

if [ -f "$SWAY_ROOTFS_CACHE" ]; then
    echo "Using cached sway rootfs ($SWAY_ROOTFS_CACHE)..."
    cd "$BUILD_DIR/rootfs"
    tar xzf "$SWAY_ROOTFS_CACHE"
else
    echo "Building sway rootfs (first run — will be cached for next time)..."
    cd "$BUILD_DIR/rootfs"
    tar xzf "$CACHE_DIR/$ALPINE_ROOTFS"

    # Install Wayland packages via chroot
    cp /etc/resolv.conf etc/resolv.conf 2>/dev/null || echo "nameserver 8.8.8.8" > etc/resolv.conf
    mkdir -p etc/apk
    echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/main" > etc/apk/repositories
    echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/community" >> etc/apk/repositories

    mount --bind /proc "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
    mount --bind /dev  "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

    chroot "$BUILD_DIR/rootfs" /sbin/apk add --no-cache \
        dbus dbus-libs \
        sway swaybg \
        seatd \
        foot \
        mesa-dri-gallium mesa-gbm \
        libinput \
        xkeyboard-config \
        eudev \
        || {
        echo ""
        echo "ERROR: Failed to install packages inside Alpine chroot."
        echo "  This needs root privileges. Try: sudo test/alpine/boot-wayland.sh"
        umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
        umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true
        rm -rf "$BUILD_DIR"
        exit 1
    }

    umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
    umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

    # Cache for future runs
    echo "Caching sway rootfs..."
    cd "$BUILD_DIR/rootfs"
    tar czf "$SWAY_ROOTFS_CACHE" .
    echo "Cached at $SWAY_ROOTFS_CACHE ($(du -sh "$SWAY_ROOTFS_CACHE" | cut -f1))"
fi

cd "$BUILD_DIR/rootfs"

# Install dynamod binaries (always fresh — not cached)
echo "Installing dynamod..."
install -Dm755 "$ZIG_OUT/dynamod-init"      sbin/dynamod-init
install -Dm755 "$CARGO_OUT/dynamod-svmgr"   usr/lib/dynamod/dynamod-svmgr
install -Dm755 "$CARGO_OUT/dynamodctl"       usr/bin/dynamodctl
install -Dm755 "$CARGO_OUT/dynamod-logd"     usr/lib/dynamod/dynamod-logd
install -Dm755 "$CARGO_OUT/dynamod-logind"      usr/lib/dynamod/dynamod-logind
install -Dm755 "$CARGO_OUT/dynamod-sd1bridge"   usr/lib/dynamod/dynamod-sd1bridge
install -Dm755 "$CARGO_OUT/dynamod-hostnamed"   usr/lib/dynamod/dynamod-hostnamed

# Install configs
mkdir -p etc/dynamod/services etc/dynamod/supervisors
cp "$PROJECT_ROOT/config/supervisors/"*.toml etc/dynamod/supervisors/

for svc in fstab-mount modules-load mdev-coldplug bootmisc hostname \
           network sysctl syslog dynamod-logd; do
    if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
        cp "$PROJECT_ROOT/config/services/${svc}.toml" etc/dynamod/services/
    fi
done

# Install D-Bus policy files
mkdir -p usr/share/dbus-1/system.d
cp "$PROJECT_ROOT/config/dbus-1/"*.conf usr/share/dbus-1/system.d/

# D-Bus system config
if [ ! -f etc/dbus-1/system.conf ]; then
    cat > etc/dbus-1/system.conf <<'DBUSCONF'
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
fi

# Install the sway session startup script
install -Dm755 "$SCRIPT_DIR/scripts/sway-session.sh" opt/sway-session.sh

# Create a service that starts the Wayland session after boot
cat > etc/dynamod/services/wayland-session.toml <<'EOF'
[service]
name = "wayland-session"
exec = ["/opt/sway-session.sh"]
type = "simple"

[restart]
policy = "permanent"
delay = "2s"
max-restarts = 3
max-restart-window = "30s"

[readiness]
type = "none"

[dependencies]
after = ["bootmisc", "mdev-coldplug"]
EOF

# System setup
echo "dynamod-test" > etc/hostname
echo "127.0.0.1 localhost dynamod-test" > etc/hosts
sed -i 's|^root:.*|root::0:0:root:/root:/bin/sh|' etc/passwd

# Busybox symlinks
for cmd in getty agetty mount umount modprobe sysctl mdev ip hostname \
           syslogd sh cat echo mkdir chmod rm true false kill ps sleep \
           grep wc seq; do
    [ ! -e "sbin/$cmd" ] && [ ! -e "bin/$cmd" ] && \
        ln -sf /bin/busybox "sbin/$cmd" 2>/dev/null || true
done

touch etc/modules etc/sysctl.conf
cat > etc/fstab <<'FSTAB'
# dynamod Wayland test fstab
FSTAB

mkdir -p var/log var/lib/dynamod run run/dbus run/user/0 tmp
chmod 1777 tmp
chmod 0700 run/user/0
# Ensure /var/run -> /run symlink (sd_bus/libseat looks at /var/run/dbus/)
rm -rf var/run 2>/dev/null
ln -sf /run var/run

# Build initramfs
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"
cd "$PROJECT_ROOT"

INITRAMFS_SIZE=$(du -sh "$BUILD_DIR/initramfs.gz" | cut -f1)
echo "Initramfs size: $INITRAMFS_SIZE"
echo ""
echo "Launching QEMU with Wayland/sway..."
echo "  - If sway starts successfully, you'll see a terminal window."
echo "  - This proves TakeDevice works (sway acquired DRM via dynamod-logind)."
echo "  - Press Super+Shift+E inside sway to exit."
echo "  - Close the QEMU window to stop."
echo ""

# Launch QEMU with KVM + virtio-gpu for DRM/Wayland support
QEMU_EXTRA=""
if [ -w /dev/kvm ]; then
    QEMU_EXTRA="-enable-kvm -cpu host"
    echo "  KVM: enabled"
else
    echo "  WARNING: /dev/kvm not available, running without KVM (very slow)"
fi

# Detect best display backend: try sdl+gl, then gtk+gl, then plain gtk
# EGL with GTK can fail on some Wayland hosts, so prefer SDL
DISPLAY_OPTS=""
if qemu-system-x86_64 -display help 2>&1 | grep -q "sdl"; then
    DISPLAY_OPTS="-device virtio-vga-gl -display sdl,gl=on"
    echo "  Display: SDL with GL"
elif qemu-system-x86_64 -display help 2>&1 | grep -q "gtk"; then
    # Try gtk without gl (avoids eglCreateWindowSurface crash)
    DISPLAY_OPTS="-device virtio-vga -display gtk"
    echo "  Display: GTK (no GL passthrough — sway will use software rendering)"
else
    DISPLAY_OPTS="-device virtio-vga -display gtk"
    echo "  Display: GTK fallback"
fi

SERIAL_LOG="$BUILD_DIR/serial.log"

qemu-system-x86_64 \
    $QEMU_EXTRA \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=ttyS0 console=tty0 rdinit=/sbin/dynamod-init panic=5" \
    -m 1024M \
    -smp 2 \
    $DISPLAY_OPTS \
    -device virtio-keyboard-pci \
    -device virtio-mouse-pci \
    -serial file:"$SERIAL_LOG" \
    -no-reboot &

QEMU_PID=$!
echo "QEMU running (PID $QEMU_PID). Close the window to stop."
echo "Serial log: $SERIAL_LOG"
echo ""

# Tail the serial log in the background so we can see VM output in the terminal
tail -f "$SERIAL_LOG" 2>/dev/null &
TAIL_PID=$!

wait $QEMU_PID 2>/dev/null || true
kill $TAIL_PID 2>/dev/null || true

echo ""
echo "=== Full serial log ==="
cat "$SERIAL_LOG" 2>/dev/null
echo "=== End serial log ==="

rm -rf "$BUILD_DIR"
echo "Done."
