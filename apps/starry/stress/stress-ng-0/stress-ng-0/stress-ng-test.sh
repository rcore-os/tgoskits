#!/bin/sh

echo "=== stress-ng cpu test ==="
stress-ng --cpu 8 --timeout 10s
CPU_RET=$?

echo "=== stress-ng sigsegv test ==="
stress-ng --sigsegv 8 --sigsegv-ops 1000
SIGSEGV_RET=$?

echo "=== results ==="
echo "cpu test exit: $CPU_RET"
echo "sigsegv test exit: $SIGSEGV_RET"

if [ "$CPU_RET" -eq 0 ]; then
    echo "STRESS_NG_TEST_PASSED"
else
    echo "STRESS_NG_TEST_FAILED"
fi
