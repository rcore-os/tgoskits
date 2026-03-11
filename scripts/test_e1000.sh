#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
QEMU_CONFIG="$PROJECT_ROOT/driver/eth/intel/qemu.toml"

cd "$PROJECT_ROOT" || exit 1

cargo t --manifest-path ./driver/eth/intel/Cargo.toml \
	--test test_e1000 \
	--target aarch64-unknown-none-softfloat \
	--config "$QEMU_CONFIG" \
	-- \
	--show-output
