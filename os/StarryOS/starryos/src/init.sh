#!/bin/sh

export HOME=/root
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export DEBIAN_FRONTEND=noninteractive

printf '\033[96m\033[1mWelcome to Starry OS!\033[0m\n'
env
echo

printf 'Use \033[1m\033[3mapt\033[0m to install packages.\n'
echo

# Do your initialization here!

cd ~

# echo "=== Test: apt update ==="
# apt update 2>&1
# echo "=== apt update exit=$? ==="

# echo "=== Test: apt install hello ==="
# apt install hello -y 2>&1
# echo "=== apt install hello exit=$? ==="

# echo; echo "=== Running hello ==="
# hello
# echo "=== hello exit=$? ==="

# Use bash if available, otherwise fall back to sh
exec /bin/bash -l
