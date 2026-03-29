#!/bin/sh
# Headless D-Bus integration test for dynamod's systemd-mimic feature.
#
# Boots Alpine + dynamod in QEMU, starts dbus + mimic daemons, and runs
# busctl-based tests to verify all D-Bus interfaces respond correctly.
#
# Prerequisites:
#   - qemu-system-x86_64
#   - wget or curl
#   - A Linux kernel bzImage (auto-detected or set KERNEL=)
#
# Usage:
#   test/alpine/test-dbus.sh
#   KERNEL=/path/to/bzImage test/alpine/test-dbus.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-dbus"
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

QEMU_TIMEOUT="${QEMU_TIMEOUT:-120}"

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

echo "=== dynamod D-Bus Integration Test ==="
echo "Kernel:  $KERNEL"
echo "Alpine:  $ALPINE_RELEASE"
echo "Timeout: ${QEMU_TIMEOUT}s"
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

# Install dynamod core binaries
echo "Installing dynamod..."
install -Dm755 "$ZIG_OUT/dynamod-init"      sbin/dynamod-init
install -Dm755 "$CARGO_OUT/dynamod-svmgr"   usr/lib/dynamod/dynamod-svmgr
install -Dm755 "$CARGO_OUT/dynamodctl"       usr/bin/dynamodctl
install -Dm755 "$CARGO_OUT/dynamod-logd"     usr/lib/dynamod/dynamod-logd

# Install systemd-mimic binaries
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

# Install D-Bus packages into the rootfs.
# Alpine minirootfs includes apk, so we chroot into it and run apk from there.
# This works on any host OS (Fedora, Arch, Debian, etc.).
echo "Installing D-Bus packages into rootfs..."
cp /etc/resolv.conf etc/resolv.conf 2>/dev/null || echo "nameserver 8.8.8.8" > etc/resolv.conf
mkdir -p etc/apk
echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/main" > etc/apk/repositories
echo "${ALPINE_MIRROR}/v${ALPINE_VERSION}/community" >> etc/apk/repositories

# Mount /proc and /dev so apk works inside the chroot
mount --bind /proc "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
mount --bind /dev  "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

chroot "$BUILD_DIR/rootfs" /sbin/apk add --no-cache dbus dbus-libs || {
    echo "ERROR: Failed to install dbus inside Alpine chroot."
    echo "  This may need root privileges. Try: sudo test/alpine/test-dbus.sh"
    umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
    umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true
    rm -rf "$BUILD_DIR"
    exit 1
}

umount "$BUILD_DIR/rootfs/proc" 2>/dev/null || true
umount "$BUILD_DIR/rootfs/dev"  2>/dev/null || true

# Verify dbus-daemon was installed
if [ ! -f "$BUILD_DIR/rootfs/usr/bin/dbus-daemon" ]; then
    echo "ERROR: dbus-daemon not found after install"
    rm -rf "$BUILD_DIR"
    exit 1
fi
echo "D-Bus installed successfully."

# Install D-Bus policy files
mkdir -p usr/share/dbus-1/system.d etc/dbus-1
cp "$PROJECT_ROOT/config/dbus-1/"*.conf usr/share/dbus-1/system.d/

# Create D-Bus system bus configuration if not present
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

# Create the D-Bus smoketest service that runs after boot
install -Dm755 "$SCRIPT_DIR/scripts/dbus-smoketest.sh" opt/dbus-smoketest.sh

cat > etc/dynamod/services/dbus-smoketest.toml <<'EOF'
[service]
name = "dbus-smoketest"
exec = ["/opt/dbus-smoketest.sh"]
type = "oneshot"

[restart]
policy = "temporary"

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
# dynamod D-Bus test fstab
FSTAB

mkdir -p var/log var/lib/dynamod run run/dbus tmp
chmod 1777 tmp

# Build initramfs
echo "Building initramfs..."
find . | cpio -o -H newc --quiet 2>/dev/null | gzip > "$BUILD_DIR/initramfs.gz"

cd "$PROJECT_ROOT"

INITRAMFS_SIZE=$(du -sh "$BUILD_DIR/initramfs.gz" | cut -f1)
echo "Initramfs size: $INITRAMFS_SIZE"
echo ""
echo "Booting QEMU (timeout: ${QEMU_TIMEOUT}s)..."
echo "---"

# Boot QEMU — capture output to check for test results
OUTPUT_FILE="$BUILD_DIR/output.log"

timeout --foreground "$QEMU_TIMEOUT" \
    qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -append "console=ttyS0 earlyprintk=ttyS0 rdinit=/sbin/dynamod-init panic=5" \
    -nographic \
    -no-reboot \
    -m 512M \
    -smp 2 \
    2>&1 | tee "$OUTPUT_FILE" || true

echo ""
echo "---"

# Check test results
if grep -q "ALL TESTS PASSED" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  D-Bus Integration Test: ALL PASSED"
    echo "========================================="
    EXITCODE=0
elif grep -q "SOME TESTS FAILED" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  D-Bus Integration Test: SOME FAILED"
    echo "========================================="
    EXITCODE=1
elif grep -q "TEST_COMPLETE" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  D-Bus Integration Test: COMPLETED"
    echo "========================================="
    EXITCODE=0
else
    echo ""
    echo "========================================="
    echo "  D-Bus Integration Test: DID NOT COMPLETE"
    echo "  (timeout or boot failure)"
    echo "========================================="
    EXITCODE=2
fi

# Cleanup
rm -rf "$BUILD_DIR"
exit $EXITCODE
