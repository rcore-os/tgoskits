#!/bin/sh
# Static checks for StarryOS syscall probe tooling (no QEMU required).
# See docs/starryos-syscall-commit-strategy.md
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== extract + catalog check =="
python3 scripts/extract_starry_syscalls.py --check-catalog docs/starryos-syscall-catalog.yaml

echo "== probe path coverage =="
python3 scripts/check_probe_coverage.py

echo "== compat matrix vs probes (incl. guest-alpine323 golden) =="
python3 scripts/check_compat_matrix.py --require-guest-golden

echo "== structured CASE selftest =="
"$ROOT/test-suit/starryos/scripts/selftest-structured-cases.sh"

echo "== shell syntax =="
for f in "$ROOT/test-suit/starryos/scripts/"*.sh; do
  [ -f "$f" ] || continue
  sh -n "$f"
done
sh -n "$ROOT/scripts/starryos-probes-ci.sh"

echo "== OK: starryos-probes-ci static checks passed =="

if command -v riscv64-linux-musl-gcc >/dev/null 2>&1; then
  echo "== cross build probes (musl) =="
  CC=riscv64-linux-musl-gcc "$ROOT/test-suit/starryos/scripts/build-probes.sh"
  echo "== OK: probe build passed =="
elif command -v riscv64-linux-gnu-gcc >/dev/null 2>&1; then
  echo "== cross build probes (gnu, e.g. Ubuntu CI) =="
  CC=riscv64-linux-gnu-gcc "$ROOT/test-suit/starryos/scripts/build-probes.sh"
  echo "== OK: probe build passed =="
else
  echo "SKIP: no riscv64-linux-musl-gcc or riscv64-linux-gnu-gcc (cross build)"
fi
