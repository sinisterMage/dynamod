#!/bin/sh
# Test dynamod's initramfs-to-rootfs transition with a real ext4 disk.
#
# Boots QEMU with:
#   - Kernel + initramfs (contains only dynamod-init + busybox)
#   - virtio-blk disk with ext4 root (contains Alpine + dynamod)
#
# The initramfs dynamod-init detects root=/dev/vda1 on the cmdline,
# mounts it, performs switch_root, and re-execs itself from the real
# root filesystem.
#
# Prerequisites:
#   - qemu-system-x86_64
#   - sfdisk, mkfs.ext4, losetup (from util-linux)
#   - A Linux kernel with: EXT4, VIRTIO_BLK, DRM (optional)
#
# Usage:
#   sudo test/alpine/boot-disk.sh
#   KERNEL=/path/to/bzImage sudo test/alpine/boot-disk.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build-disk"

QEMU_TIMEOUT="${QEMU_TIMEOUT:-90}"

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
    echo "  Needs: CONFIG_EXT4_FS=y CONFIG_VIRTIO_BLK=y"
    exit 1
fi

echo "=== dynamod Disk Boot Test ==="
echo "Kernel:  $KERNEL"
echo "Timeout: ${QEMU_TIMEOUT}s"
echo ""

# Build the disk image
echo "Building disk image..."
"$PROJECT_ROOT/tools/mkimage.sh" "$BUILD_DIR"

echo ""
echo "Booting QEMU with initramfs -> ext4 rootfs transition..."
echo "Expected sequence:"
echo "  1. dynamod-init starts in initramfs"
echo "  2. Detects root=/dev/vda1, mounts ext4"
echo "  3. switch_root to /newroot"
echo "  4. dynamod-init re-execs from real rootfs"
echo "  5. Normal boot: svmgr starts services"
echo "---"

QEMU_EXTRA=""
if [ -w /dev/kvm ]; then
    QEMU_EXTRA="-enable-kvm -cpu host"
fi

timeout --foreground "$QEMU_TIMEOUT" \
    qemu-system-x86_64 \
    $QEMU_EXTRA \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.gz" \
    -drive file="$BUILD_DIR/disk.img",format=raw,if=virtio \
    -append "console=ttyS0 earlyprintk=ttyS0 root=/dev/vda1 rootfstype=ext4 rootwait rdinit=/sbin/dynamod-init panic=5" \
    -nographic \
    -no-reboot \
    -m 512M \
    -smp 1 \
    || true

echo ""
echo "---"
echo "QEMU exited. Check output for:"
echo "  - 'initramfs: root=/dev/vda1'"
echo "  - 'mounted /dev/vda1 on /newroot'"
echo "  - 'switching root to /newroot'"
echo "  - 'dynamod-init starting (PID 1)' (appears twice)"
echo "  - 'dynamod-svmgr starting'"

# Cleanup
rm -rf "$BUILD_DIR"
echo "Done."
