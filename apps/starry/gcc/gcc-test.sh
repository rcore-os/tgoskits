#!/bin/sh
set -eu

echo "=== installing gcc toolchain ==="
apk update
apk add gcc musl-dev binutils

echo "=== compiling test-gcc ==="
gcc -Wall -Wextra -Werror -o /usr/bin/test-gcc /usr/src/test-gcc/main.c

echo "=== running test-gcc ==="
exec /usr/bin/test-gcc
