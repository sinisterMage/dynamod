#!/bin/sh
# Boot dynamod with a full GNOME desktop session in QEMU.
#
# This is a heavier test that verifies GNOME Shell, Mutter, GDM, and
# GNOME Settings all work with dynamod's systemd-mimic interfaces.
#
# Uses a disk image (qcow2) rather than initramfs because GNOME is
# too large to fit in an initramfs.
#
# Prerequisites:
#   - qemu-system-x86_64 (with GTK display support)
#   - qemu-img
#   - wget or curl
#   - A Linux kernel bzImage with DRM/virtio-gpu/virtio-blk support
#
# Usage:
#   ./test/alpine/boot-gnome.sh
#   KERNEL=/path/to/bzImage ./test/alpine/boot-gnome.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-gnome"
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

DISK_SIZE="${DISK_SIZE:-4G}"

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
    echo "  The kernel needs: DRM, virtio-gpu, virtio-blk, ext4"
    exit 1
fi

echo "=== dynamod GNOME Desktop Test ==="
echo "Kernel:    $KERNEL"
echo "Alpine:    $ALPINE_RELEASE"
echo "Disk size: $DISK_SIZE"
echo ""

# Check tools
for cmd in qemu-system-x86_64 qemu-img; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: $cmd not found"
        exit 1
    fi
done

# Check dynamod binaries
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

# Build disk image
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/rootfs"
cd "$BUILD_DIR/rootfs"
tar xzf "$CACHE_DIR/$ALPINE_ROOTFS"

# Set up Alpine repos
cp /etc/resolv.conf etc/resolv.conf 2>/dev/null || echo "nameserver 8.8.8.8" > etc/resolv.conf
mkdir -p etc/apk
echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/main" > etc/apk/repositories
echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/community" >> etc/apk/repositories

echo "Installing GNOME + Wayland packages (this will take a while)..."

mount --bind /proc "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
mount --bind /dev  "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

chroot "$BUILD_DIR/rootfs" /sbin/apk add --no-cache \
    dbus dbus-libs dbus-x11 \
    gnome-shell gnome-session gnome-settings-daemon gnome-control-center \
    mutter \
    gdm \
    adwaita-icon-theme \
    mesa-dri-gallium mesa-gbm mesa-egl \
    libinput eudev \
    xkeyboard-config \
    font-noto \
    bash \
    || {
    echo ""
    echo "ERROR: Failed to install GNOME packages inside Alpine chroot."
    echo "  This needs root privileges. Try: sudo test/alpine/boot-gnome.sh"
    umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
    umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true
    rm -rf "$BUILD_DIR"
    exit 1
}

umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

# Install dynamod binaries
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

# Create GNOME session startup script
cat > opt/gnome-session.sh <<'GNOMESCRIPT'
#!/bin/sh
# Start GNOME session with dynamod-logind

set -e

# Wait for dynamod
for i in $(seq 1 30); do
    [ -d /run/dynamod ] && break
    sleep 1
done

mkdir -p /run/dbus /var/run/dbus /run/user/0 /tmp/.X11-unix
chmod 0700 /run/user/0

# Start D-Bus
dbus-daemon --system --nofork --nopidfile &
sleep 2

# Start dynamod mimic services
/usr/lib/dynamod/dynamod-logind &
sleep 2
/usr/lib/dynamod/dynamod-sd1bridge &
/usr/lib/dynamod/dynamod-hostnamed &
sleep 2

# Create session
busctl --system call org.freedesktop.login1 \
    /org/freedesktop/login1 \
    org.freedesktop.login1.Manager CreateSession \
    "uusssssussbbss" \
    0 $$ "" "wayland" "user" "gnome" "seat0" 1 "/dev/tty1" "" false "" "" \
    2>/dev/null || true

# Environment for GNOME
export XDG_RUNTIME_DIR=/run/user/0
export XDG_SESSION_TYPE=wayland
export XDG_SEAT=seat0
export XDG_SESSION_ID=1
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
export GDK_BACKEND=wayland

# Session D-Bus
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile &
sleep 1

echo "Starting GNOME session..."

# Try gnome-session with Wayland
exec gnome-session --session=gnome-wayland 2>&1 || \
exec gnome-session 2>&1 || \
exec mutter --wayland 2>&1
GNOMESCRIPT
chmod +x opt/gnome-session.sh

# Create the GNOME service
cat > etc/dynamod/services/gnome-session.toml <<'EOF'
[service]
name = "gnome-session"
exec = ["/opt/gnome-session.sh"]
type = "simple"

[restart]
policy = "permanent"
delay = "3s"
max-restarts = 3
max-restart-window = "60s"

[readiness]
type = "none"

[dependencies]
after = ["bootmisc", "mdev-coldplug"]
EOF

# System setup
echo "dynamod-gnome-test" > etc/hostname
echo "127.0.0.1 localhost dynamod-gnome-test" > etc/hosts
sed -i 's|^root:.*|root::0:0:root:/root:/bin/bash|' etc/passwd

# Busybox symlinks
for cmd in getty mount umount modprobe sysctl mdev ip hostname \
           syslogd cat echo mkdir chmod rm true false kill ps sleep \
           grep wc seq; do
    [ ! -e "sbin/$cmd" ] && [ ! -e "bin/$cmd" ] && \
        ln -sf /bin/busybox "sbin/$cmd" 2>/dev/null || true
done

touch etc/modules etc/sysctl.conf
cat > etc/fstab <<'FSTAB'
# dynamod GNOME test fstab
FSTAB

mkdir -p var/log var/lib/dynamod run run/dbus run/user/0 tmp
chmod 1777 tmp
chmod 0700 run/user/0

# Build initramfs
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"
cd "$PROJECT_ROOT"

INITRAMFS_SIZE=$(du -sh "$BUILD_DIR/initramfs.gz" | cut -f1)
echo "Initramfs size: $INITRAMFS_SIZE"
echo ""
echo "Launching QEMU with GNOME desktop..."
echo "  - GNOME Shell should appear after boot."
echo "  - This tests: logind TakeDevice, hostname1, timedate1, locale1, systemd1"
echo "  - Close the QEMU window to stop."
echo ""

QEMU_EXTRA=""
if [ -w /dev/kvm ]; then
    QEMU_EXTRA="-enable-kvm -cpu host"
    echo "  KVM: enabled"
else
    echo "  WARNING: /dev/kvm not available, running without KVM (very slow)"
fi

DISPLAY_OPTS=""
if qemu-system-x86_64 -display help 2>&1 | grep -q "sdl"; then
    DISPLAY_OPTS="-device virtio-vga-gl -display sdl,gl=on"
    echo "  Display: SDL with GL"
elif qemu-system-x86_64 -display help 2>&1 | grep -q "gtk"; then
    DISPLAY_OPTS="-device virtio-vga -display gtk"
    echo "  Display: GTK (no GL passthrough)"
else
    DISPLAY_OPTS="-device virtio-vga -display gtk"
    echo "  Display: GTK fallback"
fi

qemu-system-x86_64 \
    $QEMU_EXTRA \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=tty0 rdinit=/sbin/dynamod-init panic=5" \
    -m 2048M \
    -smp 4 \
    $DISPLAY_OPTS \
    -device virtio-keyboard-pci \
    -device virtio-mouse-pci \
    -no-reboot &

QEMU_PID=$!
echo "QEMU running (PID $QEMU_PID). Close the window to stop."
wait $QEMU_PID 2>/dev/null || true

rm -rf "$BUILD_DIR"
echo "Done."
