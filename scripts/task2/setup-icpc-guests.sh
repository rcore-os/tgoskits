#!/usr/bin/env bash
# Prepare Guest A/B assets for icpc-smoke (peer initramfs + client binary).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
./scripts/task2/setup-peer-initramfs.sh
./scripts/task2/setup-icpc-smoke.sh
