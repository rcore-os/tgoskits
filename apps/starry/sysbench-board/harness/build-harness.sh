#!/usr/bin/env bash
# Cross-compile the harness instruments for the board (aarch64 glibc, matching
# Ubuntu Jammy and the sysbench binary). Runs natively on an arm64 host via an
# Ubuntu 22.04 arm64 container. Output: ./cpuprobe ./membw (glibc-dynamic, libc
# deps only, same as sysbench).
#
# The full cpuprobe reads MIDR_EL1/PMCCNTR_EL0 (fork+signal-guarded). Those
# privileged reads can kill a nested-hypervisor VM rather than raising a catchable
# signal, so the host SMOKE-TEST uses a -DNO_SYSREG build (timing path only); the
# real sysreg path is exercised on the board, where the guards work normally.
#
# Requires Docker/OrbStack on PATH (e.g. export PATH="$HOME/.orbstack/bin:$PATH").
set -euo pipefail
cd "$(dirname "$0")"

docker run --rm --platform linux/arm64 -v "$PWD":/src -w /src ubuntu:22.04 bash -c '
  set -e
  export DEBIAN_FRONTEND=noninteractive
  n=0; until timeout 200 apt-get update >/dev/null 2>&1; do
    n=$((n+1)); [ "$n" -ge 3 ] && { echo "apt update failed"; exit 2; }; sleep 3; done
  apt-get install -y --no-install-recommends gcc libc6-dev >/dev/null 2>&1

  # Board binaries (full sysreg probe).
  gcc -O2 -Wall -Wextra -o cpuprobe cpuprobe.c
  gcc -O2 -Wall -Wextra -o membw   membw.c

  # Host smoke-test builds/runs (timing + memory paths only; no privileged MRS).
  gcc -O2 -Wall -Wextra -DNO_SYSREG -o cpuprobe_smoke cpuprobe.c
  echo "=== sizes ==="; ls -l cpuprobe membw
  echo "=== cpuprobe timing-path smoke (NO_SYSREG) ==="; ./cpuprobe_smoke 0
  echo "=== membw smoke ==="; ./membw 0 64
  rm -f cpuprobe_smoke
'
echo "built: $PWD/cpuprobe  $PWD/membw"
