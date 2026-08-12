#!/bin/sh
# Visual bucket approach followed by one safe arm place cycle, then exit.

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
CONDA_RKNN_PREFIX="${CONDA_RKNN_PREFIX:-/home/orangepi/miniforge3/envs/rknn}"
MODEL_PATH="${1:-${SCRIPT_DIR}/models/tennis.rknn}"
FEETECH_DEV="${2:-auto}"
UVC_INDEX="${3:-0}"
OS_NAME="$(uname -s)"
HOST_NAME="$(hostname 2>/dev/null || true)"

is_starry() {
    [ "${OS_NAME}" = "Starry" ] || [ "${HOST_NAME}" = "starry" ] || [ ! -x "${SCRIPT_DIR}/build_rk3588.sh" ]
}

if is_starry && [ -z "${RKNN_CORE_MASK:-}" ]; then
    RKNN_CORE_MASK=0
    export RKNN_CORE_MASK
fi

if [ ! -f "${MODEL_PATH}" ]; then
    echo "ERROR: model file not found: ${MODEL_PATH}" >&2
    exit 1
fi

if ! is_starry && [ -d "${CONDA_RKNN_PREFIX}" ]; then
    PKG_CONFIG_PATH="${CONDA_RKNN_PREFIX}/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
    LD_LIBRARY_PATH="${CONDA_RKNN_PREFIX}/lib:${LD_LIBRARY_PATH:-}"
    export PKG_CONFIG_PATH LD_LIBRARY_PATH
fi

if is_starry; then
    if [ ! -x "${SCRIPT_DIR}/build/tennis" ]; then
        echo "ERROR: ${SCRIPT_DIR}/build/tennis is missing or not executable." >&2
        exit 1
    fi
else
    "${SCRIPT_DIR}/build_rk3588.sh" -b Release -l INFO
fi

echo "=== LeKiwi bucket/place demo ==="
echo "The robot will search for and approach the bucket, run one place cycle, then exit."
echo "The current gripper position is preserved during startup; remove the ball for an empty test."
echo "  model       : ${MODEL_PATH}"
echo "  feetech_dev : ${FEETECH_DEV}"
echo "  uvc_index   : ${UVC_INDEX}"
echo ""

cd "${SCRIPT_DIR}" || exit 1
exec "${SCRIPT_DIR}/build/tennis" \
    "${MODEL_PATH}" "${FEETECH_DEV}" "${UVC_INDEX}" "${FEETECH_DEV}" lekiwi \
    --bucket-place-demo
