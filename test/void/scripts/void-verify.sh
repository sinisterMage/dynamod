#!/bin/sh
# Void Linux full-boot verification test.
#
# Runs inside the QEMU guest as a dynamod oneshot service.
# Tests every phase of the boot chain: GRUB, PID 1, switch_root,
# service manager, D-Bus interfaces, and graceful shutdown.
#
# Uses dbus-send (not busctl) since Void musl may not ship busctl.

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

# $1 = test name, $2 = expected substring, $3... = command
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

# $1 = test name, $2... = command that should succeed
check_ok() {
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        pass "$name"
    else
        fail "$name"
    fi
}

# Helper: call a D-Bus method and check for expected string
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

# Helper: read a D-Bus property
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

# Redirect all output to serial console so the host can see test results.
# Service stdout goes to dynamod-logd by default, not to serial.
exec > /dev/ttyS0 2>&1

echo "=== Void Linux Full-Boot Verification Test ==="
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
check_output "hostname file from real rootfs" "dynamod-void-test" cat /etc/hostname
check_ok "not in initramfs (no /newroot)" test ! -d /newroot
echo ""

# ---------------------------------------------------------------
# Phase 5: dynamod-init re-executed on real rootfs
# ---------------------------------------------------------------
echo "[Phase 5] Verifying dynamod-init re-exec..."
# Void symlinks /sbin -> /usr/sbin, so readlink resolves to /usr/sbin/dynamod-init
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
# Phase 7: Early-boot services completed
# ---------------------------------------------------------------
echo "[Phase 7] Checking early-boot services..."
sleep 3
check_output "hostname set correctly" "dynamod-void-test" hostname
check_ok "loopback interface exists" ip link show lo
if [ -f /etc/machine-id ] && [ "$(wc -c < /etc/machine-id)" -ge 32 ]; then
    pass "/etc/machine-id exists and valid"
else
    fail "/etc/machine-id missing or too short"
fi
echo ""

# ---------------------------------------------------------------
# Phase 8: D-Bus daemon started by supervisor tree
# ---------------------------------------------------------------
echo "[Phase 8] Waiting for D-Bus system bus..."
i=0
while [ "$i" -lt 30 ]; do
    [ -S /run/dbus/system_bus_socket ] && break
    i=$((i + 1))
    sleep 1
done
check_ok "D-Bus socket exists" test -S /run/dbus/system_bus_socket
# Debug: show D-Bus state
echo "  D-Bus debug:"
echo "  /run/dbus contents: $(ls -la /run/dbus/ 2>&1)"
echo "  dbus-daemon running: $(pgrep -a dbus-daemon 2>&1 || echo 'NOT RUNNING')"
echo "  dbus-send test: $(dbus-send --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1 | head -5)"
echo ""

# ---------------------------------------------------------------
# Phase 9: D-Bus mimic services registered (started by supervisor tree)
# ---------------------------------------------------------------
echo "[Phase 9] Checking D-Bus mimic service registrations..."
# Give mimic services time to start and register
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
# Phase 10: dynamodctl works
# ---------------------------------------------------------------
echo "[Phase 10] Testing dynamodctl..."
# dynamodctl may get EAGAIN if svmgr is busy; retry a few times
i=0
while [ "$i" -lt 5 ]; do
    /usr/bin/dynamodctl list >/dev/null 2>&1 && break
    i=$((i + 1))
    sleep 1
done
check_ok "dynamodctl list succeeds" /usr/bin/dynamodctl list
check_ok "dynamodctl tree succeeds" /usr/bin/dynamodctl tree
check_output "bootmisc in service list" "bootmisc" /usr/bin/dynamodctl list
check_output "dbus in service list" "dbus" /usr/bin/dynamodctl list
check_output "hostname in service list" "hostname" /usr/bin/dynamodctl list
echo ""

# ---------------------------------------------------------------
# Phase 11: D-Bus method calls and property reads
# ---------------------------------------------------------------
echo "[Phase 11] Testing D-Bus interfaces..."

# --- login1 ---
echo "  login1:"
dbus_call_check "ListSeats returns seat0" "seat0" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.ListSeats

dbus_call_check "CanPowerOff returns yes" "yes" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.CanPowerOff

dbus_call_check "CanReboot returns yes" "yes" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.CanReboot

dbus_prop_check "IdleHint property" "boolean" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.IdleHint

# CreateSession
SESSION_OUT=$(dbus-send --system --print-reply --dest=org.freedesktop.login1 \
    /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.CreateSession \
    uint32:0 uint32:$$ string:"" string:"tty" string:"user" string:"" \
    string:"seat0" uint32:1 string:"" string:"" \
    boolean:false string:"" string:"" 2>&1)
if echo "$SESSION_OUT" | grep -q "/org/freedesktop/login1/session/"; then
    pass "CreateSession returns session path"
else
    fail "CreateSession (got: $(echo "$SESSION_OUT" | head -3))"
fi

dbus_call_check "ListSessions has sessions" "/org/freedesktop/login1/session/" \
    org.freedesktop.login1 /org/freedesktop/login1 \
    org.freedesktop.login1.Manager.ListSessions

# --- systemd1 ---
echo "  systemd1:"
dbus_prop_check "Version contains dynamod" "dynamod" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.Version

dbus_prop_check "SystemState is running" "running" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.SystemState

# --- hostname1 ---
echo "  hostname1:"
dbus_prop_check "StaticHostname" "dynamod" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.StaticHostname

dbus_prop_check "KernelName is Linux" "Linux" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.KernelName

# --- timedate1 ---
echo "  timedate1:"
dbus_prop_check "Timezone readable" "string" \
    org.freedesktop.timedate1 /org/freedesktop/timedate1 \
    org.freedesktop.timedate1.Timezone

# --- locale1 ---
echo "  locale1:"
check_output "locale1 on bus" "org.freedesktop.locale1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

echo ""

# ---------------------------------------------------------------
# Phase 12: Summary and shutdown
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

# Trigger graceful shutdown via dynamodctl
sleep 2
/usr/bin/dynamodctl shutdown poweroff 2>/dev/null || true
