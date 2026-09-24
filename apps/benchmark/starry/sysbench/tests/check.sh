#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for probe in cpuprobe membw; do
    cc -O2 -Wall -Wextra -Werror "$root/harness/$probe.c" -o "$work/$probe"
done
python3 "$root/tests/test_helpers.py" --bin-dir "$work"
python3 "$root/tests/test_compare.py"

python3 "$root/tests/test_delivery.py"
