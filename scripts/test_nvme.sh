#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
QEMU_CONFIG="$PROJECT_ROOT/driver/block/nvme/qemu.toml"

cd "$PROJECT_ROOT" || exit 1

mkdir -p target
dd if=/dev/zero of=target/nvme.img bs=1M count=128 status=none

cargo t --manifest-path ./driver/block/nvme/Cargo.toml \
	--test test \
	--target aarch64-unknown-none-softfloat \
	-- \
	--show-output \
	--config "$QEMU_CONFIG"