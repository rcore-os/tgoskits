#!/bin/sh
set -eu

# Unified entry point: runs all three test levels sequentially.
# Individual scripts can still be run separately via --qemu-config.

echo "===== 1. SMOKE ====="
/bin/sh /usr/bin/mosquitto-smoke-tests.sh

echo "===== 2. NORMAL ====="
/bin/sh /usr/bin/mosquitto-tests.sh

echo "===== 3. STRESS ====="
/bin/sh /usr/bin/mosquitto-stress-tests.sh

echo "ALL MOSQUITTO TESTS PASSED"
