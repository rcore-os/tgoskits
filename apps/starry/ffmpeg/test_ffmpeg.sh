#!/bin/sh
set -eu

# Unified entry point: runs all five test levels sequentially.
# Individual scripts can still be run separately via --qemu-config.

echo "===== 1. SMOKE ====="
/bin/sh /usr/bin/ffmpeg-smoke-tests.sh

echo "===== 2. BASIC ====="
/bin/sh /usr/bin/ffmpeg-basic-tests.sh

echo "===== 3. THREAD ====="
/bin/sh /usr/bin/ffmpeg-thread-tests.sh

echo "===== 4. CODEC ====="
/bin/sh /usr/bin/ffmpeg-codec-tests.sh

echo "===== 5. NETWORK ====="
/bin/sh /usr/bin/ffmpeg-network-tests.sh

echo "===== ALL FFMPEG TESTS PASSED ====="
