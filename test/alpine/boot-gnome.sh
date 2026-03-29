#!/bin/sh
# Boot dynamod with a full GNOME desktop session in QEMU.
#
# Tests that GNOME Shell, Mutter, and GNOME Settings all work with
# dynamod's systemd-mimic D-Bus interfaces.
#
# Prerequisites:
#   - qemu-system-x86_64 (with SDL or GTK display support)
#   - wget or curl
#   - A Linux kernel with: DRM, virtio-gpu, PCI, FILE_LOCKING
#
# Usage:
#   sudo ./test/alpine/boot-gnome.sh
#   KERNEL=/path/to/bzImage sudo ./test/alpine/boot-gnome.sh

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

echo "=== dynamod GNOME Desktop Test ==="
echo "Kernel: $KERNEL"
echo "Alpine: $ALPINE_RELEASE"
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

# Cache the rootfs with GNOME packages pre-installed to avoid re-downloading
# every run. The cache is invalidated by deleting it manually or changing
# ALPINE_RELEASE.
GNOME_ROOTFS_CACHE="$CACHE_DIR/alpine-gnome-rootfs-${ALPINE_RELEASE}.tar.gz"

if [ -f "$GNOME_ROOTFS_CACHE" ]; then
    echo "Using cached GNOME rootfs ($GNOME_ROOTFS_CACHE)..."
    cd "$BUILD_DIR/rootfs"
    tar xzf "$GNOME_ROOTFS_CACHE"
else
    echo "Building GNOME rootfs (first run — will be cached for next time)..."
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
        seatd \
        gnome-shell gnome-session gnome-settings-daemon gnome-control-center \
        mutter gdm \
        polkit-elogind polkit-gnome \
        gnome-keyring \
        gcr4 \
        gsettings-desktop-schemas \
        networkmanager \
        adwaita-icon-theme \
        mesa-dri-gallium mesa-gbm mesa-egl mesa-gles \
        libinput eudev \
        xkeyboard-config \
        font-noto \
        bash \
        gnome-desktop \
        evolution-data-server \
        gnome-autoar \
        gtk4.0 \
        json-glib \
        libsoup3 \
        libsecret \
        libical \
        xdg-desktop-portal-gnome \
        pipewire wireplumber \
        gst-plugin-pipewire \
        gvfs \
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

    # Cache the rootfs for future runs
    echo "Caching GNOME rootfs for future runs..."
    cd "$BUILD_DIR/rootfs"
    tar czf "$GNOME_ROOTFS_CACHE" .
    echo "Cached at $GNOME_ROOTFS_CACHE ($(du -sh "$GNOME_ROOTFS_CACHE" | cut -f1))"
fi

cd "$BUILD_DIR/rootfs"

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

# D-Bus system config (permissive for testing)
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

# Create GNOME session startup script (applies all sway test fixes)
cat > opt/gnome-session.sh <<'GNOMESCRIPT'
#!/bin/sh
# Start GNOME session with seatd + dynamod-logind
LOG=/tmp/gnome-session.log

log() {
    echo "$@"
    echo "$@" >> "$LOG"
    echo "$@" > /dev/ttyS0 2>/dev/null || true
}

: > "$LOG"

log "=== dynamod GNOME Session Startup ==="

# Wait for dynamod
log "[gnome] Waiting for dynamod..."
i=0
while [ "$i" -lt 30 ]; do
    [ -d /run/dynamod ] && break
    i=$((i + 1))
    sleep 1
done
if [ ! -d /run/dynamod ]; then
    log "[gnome] FATAL: /run/dynamod not found"
    exit 1
fi
log "[gnome] dynamod ready"

# Fix /var/run -> /run symlink (sd_bus/libelogind needs this)
mkdir -p /run/dbus
mkdir -p /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix
rm -rf /var/run 2>/dev/null
ln -sf /run /var/run

# Start seatd (handles DRM device access for Mutter)
log "[gnome] Starting seatd..."
seatd -g video >> "$LOG" 2>&1 &
SEATD_PID=$!
sleep 1
if kill -0 "$SEATD_PID" 2>/dev/null; then
    log "[gnome] seatd PID=$SEATD_PID"
else
    log "[gnome] FATAL: seatd failed"
    exit 1
fi

# Start D-Bus system bus
log "[gnome] Starting D-Bus..."
export DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus/system_bus_socket"
dbus-daemon --system --nofork --nopidfile >> "$LOG" 2>&1 &
DBUS_PID=$!
sleep 2
if kill -0 "$DBUS_PID" 2>/dev/null; then
    log "[gnome] dbus-daemon PID=$DBUS_PID"
else
    log "[gnome] FATAL: dbus-daemon failed"
    exit 1
fi

# Start dynamod mimic services
log "[gnome] Starting dynamod-logind..."
/usr/lib/dynamod/dynamod-logind >> "$LOG" 2>&1 &
sleep 2

log "[gnome] Starting dynamod-sd1bridge..."
/usr/lib/dynamod/dynamod-sd1bridge >> "$LOG" 2>&1 &
sleep 1

log "[gnome] Starting dynamod-hostnamed..."
/usr/lib/dynamod/dynamod-hostnamed >> "$LOG" 2>&1 &
sleep 2

# Verify services on D-Bus (using dbus-send, not busctl)
NAMES=$(dbus-send --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1)
for svc in org.freedesktop.login1 org.freedesktop.systemd1 org.freedesktop.hostname1; do
    if echo "$NAMES" | grep -q "$svc"; then
        log "[gnome] $svc: registered"
    else
        log "[gnome] WARNING: $svc not on D-Bus"
    fi
done

# Create login session (using dbus-send)
log "[gnome] Creating login session..."
dbus-send --system --print-reply --dest=org.freedesktop.login1 \
    /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.CreateSession \
    uint32:0 uint32:$$ string:"" string:"wayland" string:"user" string:"gnome" \
    string:"seat0" uint32:1 string:"/dev/tty1" string:"" \
    boolean:false string:"" string:"" >> "$LOG" 2>&1 || log "[gnome] WARNING: CreateSession failed"

# Write systemd-compatible session tracking files.
# Mutter/GNOME use libelogind's sd_pid_get_session() which reads these
# files to discover the session. Without them, Mutter cannot start.
log "[gnome] Writing session tracking files..."
mkdir -p /run/systemd/sessions /run/systemd/seats /run/systemd/users
cat > /run/systemd/sessions/1 <<SESSIONFILE
# Created by dynamod-logind compatibility layer
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
DESKTOP=gnome
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

log "[gnome] Session files written to /run/systemd/"

# Set up XDG environment
export XDG_RUNTIME_DIR=/tmp/gnome-run
mkdir -p "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"
rm -f "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null

export XDG_SESSION_TYPE=wayland
export XDG_SEAT=seat0
export XDG_SESSION_ID=1
export XDG_VTNR=1
unset WAYLAND_DISPLAY 2>/dev/null || true
export GDK_BACKEND=wayland
export LIBSEAT_BACKEND=seatd
export WLR_RENDERER=pixman
export MUTTER_DEBUG_DUMMY_MODE_SPECS=1024x768

# Session D-Bus
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile >> "$LOG" 2>&1 &
sleep 1

log "[gnome] Environment:"
log "  XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
log "  LIBSEAT_BACKEND=$LIBSEAT_BACKEND"
log "  GDK_BACKEND=$GDK_BACKEND"
log "  WLR_RENDERER=$WLR_RENDERER"
log ""

# Stream log to serial in real-time so we can see GNOME output on the host
tail -f "$LOG" > /dev/ttyS0 2>/dev/null &
TAIL_PID=$!

# Start udevd — Mutter uses udev (not direct /dev/dri scanning) to find GPUs
log "[gnome] Starting udevd for GPU enumeration..."
if command -v udevd >/dev/null 2>&1; then
    udevd --daemon >> "$LOG" 2>&1
    udevadm trigger --action=add >> "$LOG" 2>&1
    udevadm settle --timeout=5 >> "$LOG" 2>&1
    log "[gnome] udevd started and devices settled"
elif command -v mdev >/dev/null 2>&1; then
    mdev -s >> "$LOG" 2>&1
    log "[gnome] mdev populated /dev"
fi

# Verify GPU is visible
log "[gnome] /dev/dri/ contents:"
ls -la /dev/dri/ 2>&1 | while read -r line; do log "  $line"; done

# Let gnome-session manage the full startup (it starts mutter/gnome-shell itself).
# Don't start mutter separately — gnome-shell IS the compositor+shell combined.
log "[gnome] Starting: gnome-shell --wayland (integrated compositor + shell)..."

# gnome-shell --wayland is the all-in-one command: mutter compositor + GNOME Shell UI.
# This avoids the TakeControl conflict that happens when starting them separately.
gnome-shell --wayland >> "$LOG" 2>&1 &
GNOME_PID=$!
sleep 3

if kill -0 "$GNOME_PID" 2>/dev/null; then
    log "[gnome] gnome-shell is running! (PID=$GNOME_PID)"
    log "[gnome] GNOME desktop should be visible now."
    log "[gnome] Close the QEMU window to exit."

    wait $GNOME_PID 2>/dev/null
    GNOME_EXIT=$?
    log "[gnome] gnome-shell exited with code: $GNOME_EXIT"
else
    log "[gnome] gnome-shell --wayland failed, output:"
    cat "$LOG" > /dev/ttyS0 2>/dev/null

    # Fallback: try gnome-session which should start gnome-shell itself
    log "[gnome] Trying: gnome-session --session=gnome-wayland..."
    gnome-session --session=gnome-wayland >> "$LOG" 2>&1
    GNOME_EXIT=$?
    log "[gnome] gnome-session exited with code: $GNOME_EXIT"
fi

kill $TAIL_PID 2>/dev/null

log ""
log "=== FULL LOG ==="
cat "$LOG" > /dev/ttyS0 2>/dev/null || true
log "=== END ==="

exit $GNOME_EXIT
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

mkdir -p var/log var/lib/dynamod run run/dbus tmp
chmod 1777 tmp
# /var/run -> /run symlink (sd_bus/libelogind needs this)
rm -rf var/run 2>/dev/null
ln -sf /run var/run

# Build initramfs
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"
cd "$PROJECT_ROOT"

INITRAMFS_SIZE=$(du -sh "$BUILD_DIR/initramfs.gz" | cut -f1)
echo "Initramfs size: $INITRAMFS_SIZE"
echo ""
echo "Launching QEMU with GNOME desktop..."
echo "  - GNOME Shell should appear after boot."
echo "  - This tests: logind, hostname1, timedate1, locale1, systemd1"
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

SERIAL_LOG="$BUILD_DIR/serial.log"

qemu-system-x86_64 \
    $QEMU_EXTRA \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=ttyS0 console=tty0 rdinit=/sbin/dynamod-init panic=5" \
    -m 4096M \
    -smp 4 \
    $DISPLAY_OPTS \
    -device virtio-keyboard-pci \
    -device virtio-mouse-pci \
    -serial file:"$SERIAL_LOG" \
    -no-reboot &

QEMU_PID=$!
echo "QEMU running (PID $QEMU_PID). Close the window to stop."
echo "Serial log: $SERIAL_LOG"
echo ""

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
