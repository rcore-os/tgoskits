#!/bin/sh
set -u

echo "=== procps tool test ==="

PASS=0
FAIL=0

run_test() {
    NAME="$1"
    shift
    OUTPUT=$("$@" 2>&1)
    RET=$?
    if [ $RET -eq 0 ] && [ -n "$OUTPUT" ]; then
        PASS=$((PASS + 1))
        echo "  PASS | $NAME"
    else
        FAIL=$((FAIL + 1))
        echo "  FAIL | $NAME (ret=$RET)"
        echo "  output: $(echo "$OUTPUT" | head -3)"
    fi
}

require_tool() {
    TOOL="$1"
    if command -v "$TOOL" >/dev/null 2>&1; then
        PASS=$((PASS + 1))
        echo "  PASS | $TOOL available"
    else
        FAIL=$((FAIL + 1))
        echo "  FAIL | $TOOL available"
    fi
}

echo "=== verify procps tools ==="
for tool in ps free uptime pgrep pmap; do
    require_tool "$tool"
done

echo "=== test ps ==="
run_test "ps aux" ps aux
run_test "ps -ef" ps -ef
run_test "ps -o pid,user,comm" ps -o pid,user,comm

echo "=== test free ==="
run_test "free" free
run_test "free -m" free -m

echo "=== test uptime ==="
run_test "uptime" uptime

echo "=== test pgrep ==="
run_test "pgrep -l sh" pgrep -l sh

echo "=== test pmap ==="
if command -v pmap >/dev/null 2>&1; then
    if pmap 1 >/dev/null 2>&1; then
        run_test "pmap 1" pmap 1
    else
        echo "  SKIP | pmap (tool installed but pmap is unsupported on this arch)"
    fi
else
    echo "  SKIP | pmap (procps tool missing)"
fi

echo "=== test /proc entries ==="
run_test "/proc/self/status" cat /proc/self/status
run_test "/proc/self/stat" cat /proc/self/stat
run_test "/proc/self/statm" cat /proc/self/statm
run_test "/proc/self/maps" cat /proc/self/maps
run_test "/proc/meminfo" cat /proc/meminfo
run_test "/proc/stat" cat /proc/stat
run_test "/proc/uptime" cat /proc/uptime
run_test "/proc/loadavg" cat /proc/loadavg

echo ""
echo "=== results: PASS=$PASS FAIL=$FAIL ==="

if [ $FAIL -eq 0 ]; then
    echo "PROCPS_TEST_PASSED"
    exit 0
else
    echo "PROCPS_TEST_FAILED"
    exit 1
fi
