#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
profile=${1:-smoke}
runner=${ORANGEPI_AXVISOR_RUNNER:-orangepi-axvisor-board-run}
host_root=${ORANGEPI_AXVISOR_HOST_ROOT:?set ORANGEPI_AXVISOR_HOST_ROOT to the board Linux root device or PARTUUID}

case "$profile" in
    smoke)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-smoke.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-smoke.toml
        ;;
    full)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus.toml
        board_config=competition/ivc/config/board-orangepi-5-plus.toml
        ;;
    *)
        echo "Usage: $0 [smoke|full]" >&2
        exit 2
        ;;
esac

if ! command -v "$runner" >/dev/null 2>&1; then
    echo "Orange Pi board runner not found: $runner" >&2
    exit 1
fi

cd "$workspace"
ORANGEPI_AXVISOR_BUILD_CONFIG=$build_config \
ORANGEPI_AXVISOR_BOARD_CONFIG=$board_config \
ORANGEPI_AXVISOR_HOST_ROOT=$host_root \
ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
    exec "$runner"
