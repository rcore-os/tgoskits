#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export STARRY_TASK23_RTOS_NAME=rtthread
export STARRY_TASK23_RTOS_IMAGE=rtthread-task2.bin
export STARRY_TASK23_RUNTIME_TAG=starry-rtthread-msix1-capture
export STARRY_TASK23_QEMU_CONFIG="scripts/test/net-dual-guest/qemu-aarch64-starry-rtthread-switch-msix1-capture.toml"
export STARRY_TASK23_RTOS_VM_CONFIG="scripts/test/net-dual-guest/vm-aarch64-p2-switch-rtthread.toml"

exec "$script_dir/run-starry-task23-scenario.sh" "$@"
