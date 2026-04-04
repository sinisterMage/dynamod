#!/bin/sh
# Comprehensive Artix Linux full-boot integration test for dynamod.
#
# Boots a complete Artix Linux system from cold: SeaBIOS -> GRUB -> kernel ->
# initramfs (dynamod-init) -> switch_root -> real Artix rootfs -> full dynamod
# service tree with eudev, D-Bus mimic daemons, seatd, and sway.
#
# Unlike the Alpine tests, this does NOT pass -kernel/-initrd to QEMU.
# The disk image has a real GRUB bootloader installed.
#
# Prerequisites:
#   - qemu-system-x86_64
#   - pacstrap (from arch-install-scripts)
#   - grub-install (grub / grub2 / grub-pc-bin)
#   - sfdisk, mkfs.ext4, losetup, mount (util-linux)
#   - cpio, gzip
#   - Root privileges (sudo)
#   - A Linux kernel bzImage (auto-detected or set KERNEL=)
#
# Usage:
#   sudo test/artix/boot-full.sh
#   sudo KERNEL=/path/to/bzImage test/artix/boot-full.sh
#   sudo QEMU_TIMEOUT=300 test/artix/boot-full.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-full"
QEMU_TIMEOUT="${QEMU_TIMEOUT:-240}"

ZIG_OUT="$PROJECT_ROOT/zig/zig-out/bin"
CARGO_OUT="$(if [ -d "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release" ]; then
    echo "$PROJECT_ROOT/rust/target/x86_64-unknown-linux-musl/release"
else
    echo "$PROJECT_ROOT/rust/target/release"
fi)"

echo "=== dynamod Artix Linux Full-Boot Integration Test ==="
echo "Timeout: ${QEMU_TIMEOUT}s"
echo ""

# ============================================================
# Checks
# ============================================================
if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This test requires root (for disk image creation)."
    echo "  Run: sudo $0"
    exit 1
fi

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "ERROR: qemu-system-x86_64 not found"
    exit 1
fi

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
# Build disk image
# ============================================================
if [ -f "$BUILD_DIR/disk.img" ] && [ "$1" != "--rebuild" ]; then
    echo "Reusing existing disk image ($BUILD_DIR/disk.img)."
    echo "  Pass --rebuild to force a fresh image."
    echo ""

    echo "Refreshing dynamod binaries on existing image..."
    REFRESH_LOOP=$(losetup --find --show --partscan "$BUILD_DIR/disk.img")
    REFRESH_MNT="$BUILD_DIR/mnt-refresh"

    refresh_cleanup() {
        umount "$REFRESH_MNT" 2>/dev/null || true
        [ -n "$REFRESH_LOOP" ] && losetup -d "$REFRESH_LOOP" 2>/dev/null || true
        rm -rf "$REFRESH_MNT" 2>/dev/null || true
    }
    trap refresh_cleanup EXIT

    PART="${REFRESH_LOOP}p2"
    i=0
    while [ ! -e "$PART" ] && [ "$i" -lt 20 ]; do
        sleep 0.25
        i=$((i + 1))
    done

    if [ -e "$PART" ]; then
        mkdir -p "$REFRESH_MNT"
        mount "$PART" "$REFRESH_MNT"

        install -Dm755 "$ZIG_OUT/dynamod-init"         "$REFRESH_MNT/sbin/dynamod-init"
        install -Dm755 "$CARGO_OUT/dynamod-svmgr"      "$REFRESH_MNT/usr/lib/dynamod/dynamod-svmgr"
        install -Dm755 "$CARGO_OUT/dynamodctl"          "$REFRESH_MNT/usr/bin/dynamodctl"
        install -Dm755 "$CARGO_OUT/dynamod-logd"        "$REFRESH_MNT/usr/lib/dynamod/dynamod-logd"
        install -Dm755 "$CARGO_OUT/dynamod-logind"      "$REFRESH_MNT/usr/lib/dynamod/dynamod-logind"
        install -Dm755 "$CARGO_OUT/dynamod-sd1bridge"   "$REFRESH_MNT/usr/lib/dynamod/dynamod-sd1bridge"
        install -Dm755 "$CARGO_OUT/dynamod-hostnamed"   "$REFRESH_MNT/usr/lib/dynamod/dynamod-hostnamed"

        # Refresh configs
        cp "$PROJECT_ROOT/config/supervisors/"*.toml "$REFRESH_MNT/etc/dynamod/supervisors/"
        for svc in fsck remount-root-rw machine-id fstab-mount modules-load \
                   bootmisc hostname network sysctl dynamod-logd \
                   dbus dynamod-logind dynamod-sd1bridge dynamod-hostnamed \
                   udev udev-coldplug; do
            if [ -f "$PROJECT_ROOT/config/services/${svc}.toml" ]; then
                cp "$PROJECT_ROOT/config/services/${svc}.toml" "$REFRESH_MNT/etc/dynamod/services/"
            fi
        done
        cp "$PROJECT_ROOT/config/dbus-1/"*.conf "$REFRESH_MNT/usr/share/dbus-1/system.d/"
        install -Dm755 "$SCRIPT_DIR/scripts/artix-verify.sh" "$REFRESH_MNT/opt/artix-verify.sh"

        cat > "$REFRESH_MNT/etc/dbus-1/system.conf" <<'DBUSCONF'
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

        # Artix-specific config overrides (must come AFTER project config copy)
        cat > "$REFRESH_MNT/etc/dynamod/services/hostname.toml" <<'HOSTNAME_SVC'
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

        cat > "$REFRESH_MNT/etc/dynamod/services/dbus.toml" <<'DBUS'
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

        # Refresh initramfs with latest dynamod-init
        if [ -d "$REFRESH_MNT/boot" ]; then
            INITRAMFS_DIR="$BUILD_DIR/initramfs-refresh"
            rm -rf "$INITRAMFS_DIR"
            mkdir -p "$INITRAMFS_DIR"/{sbin,bin,dev,proc,sys,newroot}
            cp "$ZIG_OUT/dynamod-init" "$INITRAMFS_DIR/sbin/dynamod-init"

            BUSYBOX=""
            for bb in "$REFRESH_MNT/usr/bin/busybox" "$REFRESH_MNT/bin/busybox" \
                      "$PROJECT_ROOT/test/qemu/busybox" \
                      "$(command -v busybox 2>/dev/null)"; do
                [ -f "$bb" ] && BUSYBOX="$bb" && break
            done
            if [ -n "$BUSYBOX" ]; then
                cp "$BUSYBOX" "$INITRAMFS_DIR/bin/busybox"
                for cmd in sh mdev mount umount; do
                    ln -sf busybox "$INITRAMFS_DIR/bin/$cmd"
                done
                ln -sf ../bin/mdev "$INITRAMFS_DIR/sbin/mdev"
            fi

            cd "$INITRAMFS_DIR"
            find . -print0 | cpio --null -ov --format=newc 2>/dev/null | gzip -9 > "$REFRESH_MNT/boot/initramfs-dynamod.gz"
            cd "$PROJECT_ROOT"
            rm -rf "$INITRAMFS_DIR"
        fi

        umount "$REFRESH_MNT"
        rm -rf "$REFRESH_MNT"
    fi
    losetup -d "$REFRESH_LOOP"
    REFRESH_LOOP=""
    trap - EXIT
    echo "Binaries refreshed."
    echo ""
else
    echo "Building disk image..."
    "$SCRIPT_DIR/scripts/mkimage-artix.sh" "$BUILD_DIR"
    echo ""
fi

# ============================================================
# Boot QEMU
# ============================================================
QEMU_EXTRA=""
if [ -w /dev/kvm ]; then
    QEMU_EXTRA="-enable-kvm -cpu host"
    echo "KVM: enabled"
else
    echo "KVM: not available (test will be slower)"
fi

echo "Booting QEMU (timeout: ${QEMU_TIMEOUT}s)..."
echo "---"

OUTPUT_FILE="$BUILD_DIR/output.log"

timeout --foreground "$QEMU_TIMEOUT" \
    qemu-system-x86_64 \
    $QEMU_EXTRA \
    -drive file="$BUILD_DIR/disk.img",format=raw,if=virtio \
    -device virtio-vga \
    -nographic \
    -no-reboot \
    -m 2048M \
    -smp 2 \
    2>&1 | tee "$OUTPUT_FILE" || true

echo ""
echo "---"

# ============================================================
# Parse results
# ============================================================
if grep -q "ALL TESTS PASSED" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  Artix Full-Boot Test: ALL PASSED"
    echo "========================================="
    EXITCODE=0
elif grep -q "SOME TESTS FAILED" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  Artix Full-Boot Test: SOME FAILED"
    echo "========================================="
    EXITCODE=1
elif grep -q "TEST_COMPLETE" "$OUTPUT_FILE"; then
    echo ""
    echo "========================================="
    echo "  Artix Full-Boot Test: COMPLETED"
    echo "========================================="
    EXITCODE=0
else
    echo ""
    echo "========================================="
    echo "  Artix Full-Boot Test: DID NOT COMPLETE"
    echo "  (timeout or boot failure)"
    echo "========================================="
    echo ""
    echo "Check $OUTPUT_FILE for details."
    EXITCODE=2
fi

exit $EXITCODE
