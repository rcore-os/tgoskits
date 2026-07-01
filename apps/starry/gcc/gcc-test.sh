#!/bin/sh
set -eu

echo "=== installing gcc toolchain ==="
command -v gcc >/dev/null 2>&1
command -v ld >/dev/null 2>&1

echo "=== compiling test-gcc ==="
gcc -Wall -Wextra -Werror -o /usr/bin/test-gcc /usr/src/test-gcc/main.c

echo "=== running test-gcc ==="
exec /usr/bin/test-gcc
