#!/bin/sh
# Artix Linux full-boot verification test.
#
# Runs inside the QEMU guest as a dynamod oneshot service.
# Tests every phase of the boot chain: GRUB, PID 1, switch_root,
# service manager, eudev, D-Bus interfaces, sway, and graceful shutdown.

PASS=0
FAIL=0
TOTAL=0

pass() {
    PASS=$((PASS + 1))
    TOTAL=$((TOTAL + 1))
    echo "  PASS: $1"
}

fail() {
    FAIL=$((FAIL + 1))
    TOTAL=$((TOTAL + 1))
    echo "  FAIL: $1"
}

check_output() {
    local name="$1"
    local expected="$2"
    shift 2
    local output
    output=$("$@" 2>&1) || true
    if echo "$output" | grep -q "$expected"; then
        pass "$name"
    else
        fail "$name (expected '$expected', got: $(echo "$output" | head -3))"
    fi
}

check_ok() {
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        pass "$name"
    else
        fail "$name"
    fi
}

dbus_call_check() {
    local name="$1"
    local expected="$2"
    local dest="$3"
    local path="$4"
    local method="$5"
    shift 5
    check_output "$name" "$expected" \
        dbus-send --system --print-reply --dest="$dest" "$path" "$method" "$@"
}

dbus_prop_check() {
    local name="$1"
    local expected="$2"
    local dest="$3"
    local path="$4"
    local iface_prop="$5"
    local iface="${iface_prop%.*}"
    local prop="${iface_prop##*.}"
    check_output "$name" "$expected" \
        dbus-send --system --print-reply --dest="$dest" "$path" \
        org.freedesktop.DBus.Properties.Get \
        string:"$iface" \
        string:"$prop"
}

exec > /dev/ttyS0 2>&1

echo "=== Artix Linux Full-Boot Verification Test ==="
echo ""

# ---------------------------------------------------------------
# Phase 1: GRUB loaded kernel
# ---------------------------------------------------------------
echo "[Phase 1] Verifying GRUB boot..."
check_output "kernel booted with root=/dev/vda2" "root=/dev/vda2" cat /proc/cmdline
check_output "console=ttyS0 in cmdline" "console=ttyS0" cat /proc/cmdline
check_output "rootwait in cmdline" "rootwait" cat /proc/cmdline
echo ""

# ---------------------------------------------------------------
# Phase 2: dynamod-init is PID 1
# ---------------------------------------------------------------
echo "[Phase 2] Checking dynamod-init as PID 1..."
check_output "PID 1 is dynamod-init" "dynamod-init" readlink /proc/1/exe
echo ""

# ---------------------------------------------------------------
# Phase 3: Root filesystem correctly mounted
# ---------------------------------------------------------------
echo "[Phase 3] Verifying root filesystem..."
check_output "root is /dev/vda2" "/dev/vda2" mount
check_output "root is ext4" "ext4" mount
echo ""

# ---------------------------------------------------------------
# Phase 4: switch_root completed
# ---------------------------------------------------------------
echo "[Phase 4] Verifying switch_root completed..."
check_output "hostname file from real rootfs" "dynamod-artix-test" cat /etc/hostname
check_ok "not in initramfs (no /newroot)" test ! -d /newroot
echo ""

# ---------------------------------------------------------------
# Phase 5: dynamod-init re-executed on real rootfs
# ---------------------------------------------------------------
echo "[Phase 5] Verifying dynamod-init re-exec..."
check_output "PID 1 binary from real rootfs" "dynamod-init" readlink /proc/1/exe
check_ok "/run/dynamod exists" test -d /run/dynamod
echo ""

# ---------------------------------------------------------------
# Phase 6: dynamod-svmgr running
# ---------------------------------------------------------------
echo "[Phase 6] Checking dynamod-svmgr..."
i=0
while [ "$i" -lt 30 ]; do
    pgrep -f dynamod-svmgr >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done
check_ok "dynamod-svmgr is running" pgrep -f dynamod-svmgr
echo ""

# ---------------------------------------------------------------
# Phase 7: eudev running (NOT mdev)
# ---------------------------------------------------------------
echo "[Phase 7] Checking eudev device manager..."
i=0
while [ "$i" -lt 20 ]; do
    pgrep -x udevd >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done
check_ok "udevd is running" pgrep -x udevd
# Verify /dev/disk/by-uuid is populated (udev creates these symlinks)
sleep 2
if [ -d /dev/disk/by-uuid ] && [ "$(ls -A /dev/disk/by-uuid/ 2>/dev/null)" ]; then
    pass "/dev/disk/by-uuid populated by udev"
else
    fail "/dev/disk/by-uuid empty or missing"
fi
echo ""

# ---------------------------------------------------------------
# Phase 8: Early-boot services completed
# ---------------------------------------------------------------
echo "[Phase 8] Checking early-boot services..."
sleep 3
check_output "hostname set correctly" "dynamod-artix-test" cat /proc/sys/kernel/hostname
check_ok "loopback interface exists" ip link show lo
if [ -f /etc/machine-id ] && [ "$(wc -c < /etc/machine-id)" -ge 32 ]; then
    pass "/etc/machine-id exists and valid"
else
    fail "/etc/machine-id missing or too short"
fi
check_ok "cgroups v2 mounted" test -f /sys/fs/cgroup/cgroup.controllers
echo ""

# ---------------------------------------------------------------
# Phase 9: D-Bus daemon started
# ---------------------------------------------------------------
echo "[Phase 9] Waiting for D-Bus system bus..."
i=0
while [ "$i" -lt 30 ]; do
    [ -S /run/dbus/system_bus_socket ] && break
    i=$((i + 1))
    sleep 1
done
check_ok "D-Bus socket exists" test -S /run/dbus/system_bus_socket
echo ""

# ---------------------------------------------------------------
# Phase 10: D-Bus mimic services registered
# ---------------------------------------------------------------
echo "[Phase 10] Checking D-Bus mimic service registrations..."
i=0
while [ "$i" -lt 30 ]; do
    NAMES=$(dbus-send --system --print-reply --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1)
    if echo "$NAMES" | grep -q "org.freedesktop.login1" && \
       echo "$NAMES" | grep -q "org.freedesktop.systemd1" && \
       echo "$NAMES" | grep -q "org.freedesktop.hostname1"; then
        break
    fi
    i=$((i + 1))
    sleep 1
done

for svc in org.freedesktop.login1 org.freedesktop.systemd1 org.freedesktop.hostname1; do
    if echo "$NAMES" | grep -q "$svc"; then
        pass "$svc registered on D-Bus"
    else
        fail "$svc NOT registered on D-Bus"
    fi
done
echo ""

# ---------------------------------------------------------------
# Phase 11: seatd running
# ---------------------------------------------------------------
echo "[Phase 11] Checking seatd..."
i=0
while [ "$i" -lt 15 ]; do
    pgrep -x seatd >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done
check_ok "seatd is running" pgrep -x seatd
echo ""

# ---------------------------------------------------------------
# Phase 12: sway launched
# ---------------------------------------------------------------
echo "[Phase 12] Checking sway compositor..."
i=0
while [ "$i" -lt 20 ]; do
    pgrep -x sway >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done
if pgrep -x sway >/dev/null 2>&1; then
    pass "sway compositor is running"
else
    fail "sway compositor not running (may need GPU/DRM support in kernel)"
fi
echo ""

# ---------------------------------------------------------------
# Phase 13: dynamodctl works
# ---------------------------------------------------------------
echo "[Phase 13] Testing dynamodctl..."
sleep 5
i=0
while [ "$i" -lt 20 ]; do
    timeout 5 /usr/bin/dynamodctl list >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 2
done
check_ok "dynamodctl list succeeds" timeout 10 /usr/bin/dynamodctl list
check_ok "dynamodctl tree succeeds" timeout 10 /usr/bin/dynamodctl tree
check_output "udev-coldplug in service list" "udev-coldplug" timeout 10 /usr/bin/dynamodctl list
check_output "dbus in service list" "dbus" timeout 10 /usr/bin/dynamodctl list
check_output "seatd in service list" "seatd" timeout 10 /usr/bin/dynamodctl list
echo ""

# ---------------------------------------------------------------
# Phase 14: D-Bus interface tests
# ---------------------------------------------------------------
echo "[Phase 14] Testing D-Bus interfaces..."

echo "  login1:"
dbus_call_check "ListSeats returns seat0" "seat0" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.ListSeats

dbus_call_check "CanPowerOff returns yes" "yes" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.CanPowerOff

echo "  systemd1:"
dbus_prop_check "Version contains dynamod" "dynamod" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.Version

dbus_prop_check "SystemState is running" "running" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.SystemState

echo "  hostname1:"
dbus_prop_check "StaticHostname" "dynamod" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.StaticHostname

dbus_prop_check "KernelName is Linux" "Linux" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.KernelName

echo ""

# ---------------------------------------------------------------
# Phase 15: Summary and shutdown
# ---------------------------------------------------------------
echo "================================"
echo "Results: $PASS passed, $FAIL failed (out of $TOTAL)"
echo "================================"

if [ "$FAIL" -eq 0 ]; then
    echo "ALL TESTS PASSED"
else
    echo "SOME TESTS FAILED"
fi
echo "TEST_COMPLETE"

sleep 2
/usr/bin/dynamodctl shutdown poweroff 2>/dev/null || true
