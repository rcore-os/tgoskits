#!/usr/bin/env bash
# Task 1 phase-3 stress matrix helper: mixed guests + documented stress commands.
set -euo pipefail

AXVISOR_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../os/axvisor" && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"

info() { echo "[task1] $*"; }

info "=== Task 1 stress matrix (manual long-run) ==="
info "1) Build RT guest benchmark image"
info "   cd ${AXVISOR_ROOT} && ./scripts/task1/build-arceos-rt-guest.sh"
info "2) Prepare QEMU configs"
info "   cd ${AXVISOR_ROOT} && ./scripts/task1/setup-qemu-aarch64.sh"
info "3) Launch mixed partition (Linux pCPU1-2 + RT pCPU3)"
info "   cd ${AXVISOR_ROOT} && ./scripts/task1/run-mixed.sh"
info "4) In Linux guest shell, start stress (example 30 min):"
info "   stress-ng --cpu 2 --vm 1 --fork 4 --timeout 1800s"
info "5) Observe RT guest serial for RT_LATENCY lines; redirect host log to CSV if needed."
info
info "For automated round-1 (idle compare + mixed stress long-run + report):"
info "   ${TGOSKITS_ROOT}/scripts/task1/run-mixed-stress-round1.sh"
info
info "For 30-minute RT-only guest sampling, rebuild with long mode:"
info "   cd ${TGOSKITS_ROOT}"
info "   cargo xtask arceos build --arch aarch64 -p arceos-test-suit -c test-suit/arceos/rust/build-aarch64-rt-latency-long-guest.toml"
info "   cd ${AXVISOR_ROOT} && ./scripts/task1/build-arceos-rt-guest.sh"
