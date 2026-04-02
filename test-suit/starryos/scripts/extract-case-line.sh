#!/usr/bin/sh
# Print the first line starting with "CASE " (guest serial / probe stdout).
# Strips CR (\r) for Windows/serial captures.
# Usage: extract-case-line.sh [file]
#        qemu-riscv64 ./probe | extract-case-line.sh
set -eu
if [ "$#" -ge 1 ]; then
  grep -m1 '^CASE ' "$1" | tr -d '\r' || true
else
  tr -d '\r' | grep -m1 '^CASE ' || true
fi
