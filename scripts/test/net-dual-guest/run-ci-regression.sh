#!/usr/bin/env bash
set -euo pipefail

# Fast, hardware-independent gate for the Task-2/Task-3 endpoint contracts.
# Full AArch64 dual-Guest runs remain explicit QEMU evidence jobs; this gate
# makes protocol and host/guest interoperability regressions visible in the
# repository's normal CI path.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repo_root"

echo "TASK2_CI_GATE_START"
cargo test -p task2-net-protocol
cargo test -p arceos-task2-net --no-default-features
python3 -m unittest discover -s scripts/test/net-dual-guest -p 'test_*.py'
TASK3_CONTROL_LOOP=1 TASK3_MODEL=cnn cargo check -p arceos-task2-net --no-default-features
TASK3_CONTROL_LOOP=1 TASK3_MODEL=yolo \
  TASK3_MODEL_PATH=/usr/share/task3-yolo \
  cargo check -p arceos-task2-net --no-default-features
echo "TASK2_CI_GATE_PASS"
