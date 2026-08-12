#!/bin/sh
# Build when running on Linux, then run the vision-only RKNN check.
# Starry uses the binary already deployed through the shared root filesystem.
#
# This command intentionally does not initialize motors or the arm. It runs the
# existing `tennis test-yolo` subcommand, captures one UVC frame, runs RKNN
# detection, and writes:
#   - capture.jpg
#   - result.jpg
#
# Usage:
#   ./run_vision_once.sh [model.rknn] [uvc_index]

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$SCRIPT_DIR"

CONDA_RKNN_PREFIX="${CONDA_RKNN_PREFIX:-/home/orangepi/miniforge3/envs/rknn}"
RKNN_CORE_MASK="${RKNN_CORE_MASK:-0}"
TENNIS_BIN="${TENNIS_BIN:-$SCRIPT_DIR/build/tennis}"
export RKNN_CORE_MASK

if [ "$#" -gt 2 ]; then
    echo "Usage: $0 [model.rknn] [uvc_index]" >&2
    exit 2
fi

MODEL_PATH="${1:-models/tennis.rknn}"
UVC_INDEX="${2:-0}"

case "$MODEL_PATH" in
    /*) ;;
    *) MODEL_PATH="$SCRIPT_DIR/$MODEL_PATH" ;;
esac

if [ ! -f "$MODEL_PATH" ]; then
    echo "ERROR: model file not found: ${MODEL_PATH}" >&2
    exit 1
fi

is_starry() {
    if [ "${HOSTNAME:-}" = "starry" ]; then
        return 0
    fi
    os_name=$(uname -s 2>/dev/null || true)
    [ "$os_name" = "Starry" ] || [ "$os_name" = "StarryOS" ]
}

if is_starry; then
    PLATFORM=Starry
    ACTION="run existing binary"
elif [ ! -x "$SCRIPT_DIR/build_rk3588.sh" ]; then
    PLATFORM=Linux
    ACTION="run prebuilt binary"
else
    PLATFORM=Linux
    ACTION="build native binary, then run"
fi

echo "=== Vision-only check ==="
echo "  platform  : ${PLATFORM}"
echo "  model     : ${MODEL_PATH}"
echo "  uvc_index : ${UVC_INDEX}"
echo "  NPU core  : ${RKNN_CORE_MASK}"
echo "  action    : ${ACTION} test-yolo once"
echo ""

if is_starry || [ ! -x "$SCRIPT_DIR/build_rk3588.sh" ]; then
    if [ ! -x "$TENNIS_BIN" ]; then
        echo "ERROR: existing binary not found: $TENNIS_BIN" >&2
        echo "Deploy the prebuilt package first." >&2
        exit 1
    fi
else
    echo "  conda env : ${CONDA_RKNN_PREFIX}"
    if [ -d "$CONDA_RKNN_PREFIX" ]; then
        export PKG_CONFIG_PATH="${CONDA_RKNN_PREFIX}/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
        export LD_LIBRARY_PATH="${CONDA_RKNN_PREFIX}/lib:${LD_LIBRARY_PATH:-}"
    fi

    "$SCRIPT_DIR/build_rk3588.sh" -b Release -l INFO
fi

echo ""
echo "=== Running ==="
echo "  ${TENNIS_BIN} test-yolo ${MODEL_PATH} ${UVC_INDEX}"
echo ""

"$TENNIS_BIN" test-yolo "$MODEL_PATH" "$UVC_INDEX"

echo ""
echo "=== Done ==="
echo "  raw frame : ${SCRIPT_DIR}/capture.jpg"
echo "  result    : ${SCRIPT_DIR}/result.jpg"
