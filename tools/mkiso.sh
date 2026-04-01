#!/bin/sh
# Build dynamod-live.iso: ISO9660 + Joliet (-J) volume containing live/root.squashfs for dynamod.live boot.
#
# Expects an existing Alpine disk image from tools/mkimage.sh (disk.img with ext4 root).
# The squashfs is built from that root filesystem (same content as the disk test image).
#
# Usage:
#   sudo tools/mkiso.sh [build-dir]
#
# Default build-dir: <repo>/test/alpine/build-disk
#
# Prerequisites:
#   - mksquashfs (squashfs-tools), xorriso, mount, losetup (util-linux)
#   - Run `make` and `sudo tools/mkimage.sh [build-dir]` first
#
# QEMU example (kernel and initrd outside the ISO; ISO only carries squashfs):
#   qemu-system-x86_64 -kernel /boot/vmlinuz-xxx -initrd BUILD_DIR/initramfs.gz \
#     -cdrom BUILD_DIR/dynamod-live.iso \
#     -append 'console=ttyS0 rdinit=/sbin/dynamod-init dynamod.live=1 dynamod.media=LABEL=DYNAISO dynamod.squashfs=/live/root.squashfs rootwait' \
#     -nographic -no-reboot -m 512M

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="${1:-$PROJECT_ROOT/test/alpine/build-disk}"
DISK="$BUILD_DIR/disk.img"
OUT_ISO="$BUILD_DIR/dynamod-live.iso"

for cmd in mksquashfs xorriso losetup mount umount; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: $cmd not found"
        exit 1
    fi
done

if [ ! -f "$DISK" ]; then
    echo "ERROR: $DISK not found. Run: sudo tools/mkimage.sh $BUILD_DIR"
    exit 1
fi

WORK="$(mktemp -d)"
cleanup() {
    umount -l "$WORK/mnt" 2>/dev/null || true
    if [ -n "$LOOP" ]; then
        losetup -d "$LOOP" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT INT

mkdir -p "$WORK/mnt" "$WORK/iso/live"
LOOP="$(losetup --find --show --partscan "$DISK")"
PART="${LOOP}p1"
i=0
while [ ! -e "$PART" ] && [ "$i" -lt 40 ]; do
    sleep 0.25
    i=$((i + 1))
done
if [ ! -e "$PART" ]; then
    echo "ERROR: Partition $PART did not appear"
    exit 1
fi

echo "Mounting $PART -> $WORK/mnt"
mount -o ro "$PART" "$WORK/mnt"

echo "Creating squashfs (this may take a minute)..."
mksquashfs "$WORK/mnt" "$WORK/iso/live/root.squashfs" -comp xz -noappend

umount "$WORK/mnt"
losetup -d "$LOOP"
LOOP=""

echo "Building ISO9660 image -> $OUT_ISO"
xorriso -as mkisofs \
    -o "$OUT_ISO" \
    -V DYNAISO \
    -rational-rock \
    -J \
    "$WORK/iso"

echo ""
echo "=== ISO ready ==="
echo "  $OUT_ISO"
echo ""
echo "Use with initramfs from the same build dir:"
echo "  $BUILD_DIR/initramfs.gz"
echo ""
echo "Example kernel cmdline append:"
echo "  rdinit=/sbin/dynamod-init dynamod.live=1 dynamod.media=LABEL=DYNAISO dynamod.squashfs=/live/root.squashfs rootwait"
