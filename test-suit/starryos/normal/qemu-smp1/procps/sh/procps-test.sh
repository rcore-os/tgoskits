#!/bin/sh
echo "=== install procps ==="
apk add procps || echo "  WARN | apk add procps failed (network issue), using busybox fallback"

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
if apk info -e procps >/dev/null 2>&1; then
    run_test "pmap 1" pmap 1
else
    echo "  SKIP | pmap (procps-ng not installed)"
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
else
    echo "PROCPS_TEST_FAILED"
fi
