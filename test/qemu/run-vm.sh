#!/bin/bash
# QEMU integration test for dynamod.
#
# Boots a minimal Linux system with dynamod-init as PID 1.
# Requires: qemu-system-x86_64, a Linux kernel (bzImage), busybox (static).
#
# Usage:
#   ./run-vm.sh [kernel-path]
#
# If no kernel is specified, looks for /boot/vmlinuz-* or downloads one.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"

KERNEL="${1:-}"
TIMEOUT=30

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[dynamod-test]${NC} $*"; }
warn() { echo -e "${YELLOW}[dynamod-test]${NC} $*"; }
err() { echo -e "${RED}[dynamod-test]${NC} $*" >&2; }

# Check prerequisites
for cmd in qemu-system-x86_64 cpio gzip; do
    if ! command -v "$cmd" &>/dev/null; then
        err "Required command not found: $cmd"
        exit 1
    fi
done

# Find kernel
if [ -z "$KERNEL" ]; then
    KERNEL=$(ls /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1 || true)
    if [ -z "$KERNEL" ]; then
        err "No kernel found. Pass a kernel path as argument."
        err "Usage: $0 /path/to/bzImage"
        exit 1
    fi
fi
log "Using kernel: $KERNEL"

# Build dynamod
log "Building dynamod..."
(cd "$PROJECT_DIR/zig" && zig build) || { err "Zig build failed"; exit 1; }
(cd "$PROJECT_DIR/rust" && cargo build --release) || { err "Rust build failed"; exit 1; }

# Create initramfs
log "Creating initramfs..."
mkdir -p "$BUILD_DIR/initramfs"/{bin,sbin,etc/dynamod/services,etc/dynamod/supervisors,usr/lib/dynamod,run,proc,sys,dev,tmp,var/log/dynamod,var/lib/dynamod}

# Copy dynamod binaries
cp "$PROJECT_DIR/zig/zig-out/bin/dynamod-init" "$BUILD_DIR/initramfs/sbin/init"
# Find Rust binaries (musl target uses a different path)
RUST_BIN="$PROJECT_DIR/rust/target/release"
if [ -f "$PROJECT_DIR/rust/target/x86_64-unknown-linux-musl/release/dynamod-svmgr" ]; then
    RUST_BIN="$PROJECT_DIR/rust/target/x86_64-unknown-linux-musl/release"
fi
cp "$RUST_BIN/dynamod-svmgr" "$BUILD_DIR/initramfs/usr/lib/dynamod/"
cp "$RUST_BIN/dynamodctl" "$BUILD_DIR/initramfs/bin/"
cp "$RUST_BIN/dynamod-logd" "$BUILD_DIR/initramfs/usr/lib/dynamod/"

# Copy busybox if available (for shell access)
BUSYBOX="${SCRIPT_DIR}/busybox"
if [ ! -f "$BUSYBOX" ]; then
    BUSYBOX=$(command -v busybox 2>/dev/null || true)
fi
if [ -n "$BUSYBOX" ] && [ -f "$BUSYBOX" ] && file "$BUSYBOX" | grep -q "statically linked"; then
    cp "$BUSYBOX" "$BUILD_DIR/initramfs/bin/busybox"
    # Create common symlinks
    for cmd in sh ls cat echo sleep true false kill ps mount umount mkdir; do
        ln -sf busybox "$BUILD_DIR/initramfs/bin/$cmd"
    done
    ln -sf ../bin/sh "$BUILD_DIR/initramfs/sbin/sh"
    log "Included busybox for shell access"
else
    warn "No static busybox found — VM will have no shell"
fi

# Create a simple test service
cat > "$BUILD_DIR/initramfs/etc/dynamod/services/test-hello.toml" <<'EOF'
[service]
name = "test-hello"
exec = ["/bin/echo", "dynamod boot successful!"]
type = "oneshot"

[restart]
policy = "temporary"

[readiness]
type = "none"
EOF

# Create root supervisor config
cp "$PROJECT_DIR/config/supervisors/root.toml" "$BUILD_DIR/initramfs/etc/dynamod/supervisors/"

# Create /etc/hostname
echo "dynamod-test" > "$BUILD_DIR/initramfs/etc/hostname"

# Pack initramfs
log "Packing initramfs..."
(cd "$BUILD_DIR/initramfs" && find . -print0 | cpio --null -ov --format=newc 2>/dev/null | gzip -9 > "$BUILD_DIR/initramfs.cpio.gz")

INITRAMFS_SIZE=$(du -sh "$BUILD_DIR/initramfs.cpio.gz" | cut -f1)
log "Initramfs size: $INITRAMFS_SIZE"

# Run QEMU
log "Booting QEMU (timeout ${TIMEOUT}s)..."
log "Console output:"
echo "---"

timeout --foreground "$TIMEOUT" \
    qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -initrd "$BUILD_DIR/initramfs.cpio.gz" \
    -append "console=ttyS0 earlyprintk=ttyS0 panic=1 rdinit=/sbin/init" \
    -nographic \
    -no-reboot \
    -m 256M \
    -smp 1 \
    || true

echo "---"
log "QEMU session ended."

# Live ISO smoke test (after: make, sudo tools/mkimage.sh, sudo tools/mkiso.sh):
#   qemu-system-x86_64 -kernel "$KERNEL" -initrd /path/to/build-disk/initramfs.gz \
#     -cdrom /path/to/build-disk/dynamod-live.iso \
#     -append 'console=ttyS0 rdinit=/sbin/dynamod-init dynamod.live=1 dynamod.media=LABEL=DYNAISO dynamod.squashfs=/live/root.squashfs rootwait' \
#     -nographic -no-reboot -m 512M

# Cleanup
rm -rf "$BUILD_DIR"
log "Done."
