#!/bin/sh
# D-Bus smoke test runner — executes inside the QEMU VM.
#
# Tests that dynamod-logind, dynamod-sd1bridge, and dynamod-hostnamed
# respond correctly to D-Bus method calls.
# Uses dbus-send (not busctl) since Alpine doesn't ship busctl.

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
    check_output "$name" "$expected" \
        dbus-send --system --print-reply --dest="$dest" "$path" \
        org.freedesktop.DBus.Properties.Get \
        string:"$(echo "$iface_prop" | cut -d. -f1-5)" \
        string:"$(echo "$iface_prop" | rev | cut -d. -f1 | rev)"
}

echo "=== dynamod D-Bus Smoke Test ==="
echo ""

# Wait for /run/dynamod
echo "Waiting for dynamod runtime..."
i=0
while [ "$i" -lt 30 ]; do
    [ -d /run/dynamod ] && break
    i=$((i + 1))
    sleep 1
done
if [ ! -d /run/dynamod ]; then
    echo "FATAL: /run/dynamod not found after 30s"
    exit 1
fi
echo "dynamod runtime ready."
echo ""

# --- Phase 1: Start D-Bus ---
echo "[Phase 1] Starting D-Bus system bus..."
mkdir -p /run/dbus /var/run/dbus
dbus-daemon --system --nofork --nopidfile &
DBUS_PID=$!
sleep 2
if kill -0 "$DBUS_PID" 2>/dev/null; then
    echo "  dbus-daemon running (PID $DBUS_PID)"
else
    echo "FATAL: dbus-daemon failed to start"
    exit 1
fi
echo ""

# --- Phase 2: Start mimic services ---
echo "[Phase 2] Starting systemd-mimic daemons..."
/usr/lib/dynamod/dynamod-logind &
LOGIND_PID=$!
sleep 3

/usr/lib/dynamod/dynamod-sd1bridge &
BRIDGE_PID=$!
sleep 1

/usr/lib/dynamod/dynamod-hostnamed &
HOSTNAMED_PID=$!
sleep 2

echo "  dynamod-logind    PID=$LOGIND_PID"
echo "  dynamod-sd1bridge PID=$BRIDGE_PID"
echo "  dynamod-hostnamed PID=$HOSTNAMED_PID"

# Verify they registered on the bus
echo "  Checking bus registrations..."
NAMES=$(dbus-send --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1)
for svc in org.freedesktop.login1 org.freedesktop.systemd1 org.freedesktop.hostname1; do
    if echo "$NAMES" | grep -q "$svc"; then
        echo "  $svc: registered"
    else
        echo "  $svc: NOT registered"
    fi
done
echo ""

# --- Phase 3: Test login1 ---
echo "[Phase 3] Testing org.freedesktop.login1..."

check_output "login1 on bus" "org.freedesktop.login1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

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

dbus_prop_check "seat0 CanGraphical" "boolean" \
    org.freedesktop.login1 /org/freedesktop/login1/seat/seat0 \
    org.freedesktop.login1.Seat.CanGraphical

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

echo ""

# --- Phase 4: Test systemd1 ---
echo "[Phase 4] Testing org.freedesktop.systemd1..."

check_output "systemd1 on bus" "org.freedesktop.systemd1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

dbus_prop_check "Version contains dynamod" "dynamod" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.Version

dbus_prop_check "SystemState is running" "running" \
    org.freedesktop.systemd1 /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.SystemState

echo ""

# --- Phase 5: Test hostname1/timedate1/locale1 ---
echo "[Phase 5] Testing org.freedesktop.hostname1..."

check_output "hostname1 on bus" "org.freedesktop.hostname1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

dbus_prop_check "StaticHostname" "dynamod" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.StaticHostname

dbus_prop_check "KernelName" "Linux" \
    org.freedesktop.hostname1 /org/freedesktop/hostname1 \
    org.freedesktop.hostname1.KernelName

echo ""
echo "[Phase 5b] Testing org.freedesktop.timedate1..."

check_output "timedate1 on bus" "org.freedesktop.timedate1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

dbus_prop_check "Timezone readable" "string" \
    org.freedesktop.timedate1 /org/freedesktop/timedate1 \
    org.freedesktop.timedate1.Timezone

echo ""
echo "[Phase 5c] Testing org.freedesktop.locale1..."

check_output "locale1 on bus" "org.freedesktop.locale1" \
    dbus-send --system --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.ListNames

echo ""

# --- Phase 6: machine-id ---
echo "[Phase 6] Checking /etc/machine-id..."
if [ -f /etc/machine-id ] && [ "$(wc -c < /etc/machine-id)" -ge 32 ]; then
    pass "machine-id exists ($(cat /etc/machine-id | head -c 32))"
else
    fail "machine-id missing or too short"
fi

echo ""

# --- Summary ---
echo "================================"
echo "Results: $PASS passed, $FAIL failed (out of $TOTAL)"
echo "================================"

if [ "$FAIL" -eq 0 ]; then
    echo "ALL TESTS PASSED"
else
    echo "SOME TESTS FAILED"
fi

# Clean up
kill "$LOGIND_PID" "$BRIDGE_PID" "$HOSTNAMED_PID" "$DBUS_PID" 2>/dev/null || true
sleep 1
echo "TEST_COMPLETE"
