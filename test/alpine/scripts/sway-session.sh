#!/bin/sh
# Sway Wayland session startup — executes inside the QEMU VM.
#
# Uses seatd for DRM device access (the standard approach for sway
# without systemd) and dynamod-logind for D-Bus session management.

LOG=/tmp/sway-session.log

log() {
    echo "$@"
    echo "$@" >> "$LOG"
    echo "$@" > /dev/ttyS0 2>/dev/null || true
}

: > "$LOG"

log "=== dynamod Wayland Session Startup ==="
log ""

# ---- Step 1: Wait for dynamod ----
log "[step 1] Waiting for /run/dynamod..."
i=0
while [ "$i" -lt 30 ]; do
    [ -d /run/dynamod ] && break
    i=$((i + 1))
    sleep 1
done
if [ ! -d /run/dynamod ]; then
    log "[step 1] FATAL: /run/dynamod not found after 30s"
    exit 1
fi
log "[step 1] OK"

# ---- Step 2: Create directories ----
log "[step 2] Creating runtime directories..."
mkdir -p /run/dbus /tmp/.X11-unix
# XDG_RUNTIME_DIR must exist and be owned by the user (root=0)
mkdir -p /run/user/0
chmod 0700 /run/user/0
chown 0:0 /run/user/0
rm -rf /var/run 2>/dev/null
ln -sf /run /var/run
log "[step 2] /run/user/0 exists: $(ls -ld /run/user/0)"
log "[step 2] OK"

# ---- Step 3: Start seatd ----
# seatd handles DRM/input device access for Wayland compositors.
# This is the standard approach for sway without systemd.
log "[step 3] Starting seatd..."
if ! command -v seatd >/dev/null 2>&1; then
    log "[step 3] FATAL: seatd not found — install with: apk add seatd"
    exit 1
fi
seatd -g video >> "$LOG" 2>&1 &
SEATD_PID=$!
sleep 1
if ! kill -0 "$SEATD_PID" 2>/dev/null; then
    log "[step 3] FATAL: seatd crashed"
    exit 1
fi
log "[step 3] OK — seatd PID=$SEATD_PID"

# ---- Step 4: Start D-Bus + logind (for session management) ----
log "[step 4] Starting D-Bus..."
if command -v dbus-daemon >/dev/null 2>&1; then
    dbus-daemon --system --nofork --nopidfile >> "$LOG" 2>&1 &
    DBUS_PID=$!
    sleep 2
    if kill -0 "$DBUS_PID" 2>/dev/null; then
        log "[step 4] dbus-daemon PID=$DBUS_PID"

        # Start logind for D-Bus session management
        /usr/lib/dynamod/dynamod-logind >> "$LOG" 2>&1 &
        LOGIND_PID=$!
        sleep 2
        if kill -0 "$LOGIND_PID" 2>/dev/null; then
            log "[step 4] dynamod-logind PID=$LOGIND_PID"
        else
            log "[step 4] WARNING: dynamod-logind failed (non-fatal, seatd handles device access)"
        fi
    else
        log "[step 4] WARNING: dbus-daemon failed (non-fatal for sway)"
    fi
else
    log "[step 4] WARNING: dbus-daemon not found (non-fatal for sway)"
fi
log "[step 4] OK"

# ---- Step 5: Check DRM ----
log "[step 5] Checking DRM devices..."
if [ -d /dev/dri ]; then
    ls -la /dev/dri/ 2>&1 | while read -r line; do log "  $line"; done
else
    log "[step 5] FATAL: /dev/dri/ does not exist"
    log "  Kernel needs: CONFIG_DRM=y CONFIG_DRM_VIRTIO_GPU=y"
    while true; do sleep 60; done
fi
if [ ! -e /dev/dri/card0 ]; then
    log "[step 5] FATAL: /dev/dri/card0 not found"
    while true; do sleep 60; done
fi
log "[step 5] OK — /dev/dri/card0 found"

# ---- Step 6: Set up environment ----
log "[step 6] Setting up sway environment..."
export XDG_RUNTIME_DIR=/tmp/sway-run
mkdir -p "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"
# Clean up any leftover sockets from previous attempts
rm -f "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null
export XDG_SESSION_TYPE=wayland
export XDG_SEAT=seat0
# Do NOT set WAYLAND_DISPLAY — that tells wlroots to nest inside an
# existing compositor. We ARE the compositor, so use DRM directly.
unset WAYLAND_DISPLAY 2>/dev/null || true
export WLR_BACKENDS=drm
export WLR_RENDERER=pixman
# Tell libseat to use seatd backend (not logind)
export LIBSEAT_BACKEND=seatd

log "[step 6] LIBSEAT_BACKEND=$LIBSEAT_BACKEND"
log "[step 6] WLR_RENDERER=$WLR_RENDERER"
log "[step 6] XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
log "[step 6] OK"

# ---- Step 7: sway config ----
log "[step 7] Writing sway config..."
mkdir -p /root/.config/sway
cat > /root/.config/sway/config <<'SWAYCONF'
set $term foot
exec $term
bindsym Mod4+Shift+e exit
output * bg #285577 solid_color
bar {
    status_command echo "dynamod + sway (via seatd)"
    position top
}
SWAYCONF
log "[step 7] OK"

# ---- Step 8: Check sway ----
log "[step 8] Checking sway binary and XDG_RUNTIME_DIR..."
if ! command -v sway >/dev/null 2>&1; then
    log "[step 8] FATAL: sway not found"
    exit 1
fi
log "[step 8] sway at: $(command -v sway)"
# Ensure XDG_RUNTIME_DIR exists right before launch
mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null
chmod 0700 "$XDG_RUNTIME_DIR" 2>/dev/null
log "[step 8] XDG_RUNTIME_DIR: $(ls -ld $XDG_RUNTIME_DIR 2>&1)"

# Test that we can create a Unix socket in XDG_RUNTIME_DIR
log "[step 8] Testing Unix socket creation..."
python3 -c "
import socket, os
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
p = os.environ['XDG_RUNTIME_DIR'] + '/test-sock'
try:
    os.unlink(p)
except: pass
s.bind(p)
s.close()
os.unlink(p)
print('OK')
" 2>&1 | while read -r line; do log "  $line"; done || {
    # python3 not available, try with socat or just proceed
    log "[step 8] (python3 not available for socket test)"
}

# Print ALL environment variables that sway will inherit
log "[step 8] Environment for sway:"
env 2>&1 | grep -E "XDG_|WAYLAND|WLR_|LIBSEAT|DISPLAY|DBUS_" | while read -r line; do log "  $line"; done

# ---- Step 9: Launch sway ----
log ""
log "=== [step 9] Launching sway ==="
log "  LIBSEAT_BACKEND=seatd (device access via seatd)"
log "  If you see a terminal window, it works!"
log "  Press Super+Shift+E to exit sway."
log ""

# Test: can we create files in XDG_RUNTIME_DIR?
touch "$XDG_RUNTIME_DIR/test-file" 2>&1 && {
    log "[step 9] touch test: OK"
    rm -f "$XDG_RUNTIME_DIR/test-file"
} || log "[step 9] touch test: FAILED"

log "[step 9] XDG_RUNTIME_DIR contents before sway:"
ls -la "$XDG_RUNTIME_DIR" 2>&1 | while read -r line; do log "  $line"; done

# Run sway with explicit environment, verbose logging
# Use -d flag for sway debug output
sway -d >> "$LOG" 2>&1
SWAY_EXIT=$?

log "[step 9] sway exited with code: $SWAY_EXIT"

# Dump log to serial
log ""
log "=== FULL LOG ==="
cat "$LOG" > /dev/ttyS0 2>/dev/null || true
log "=== END ==="

exit $SWAY_EXIT
