#!/bin/sh
# Full pick-and-place flow. This can move the robot and search for a bucket.

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

wait_for_starry_usb() {
    is_starry || return 0

    case "${FEETECH_DEV}" in
        auto|usb|libusb|cdc) ;;
        *) return 0 ;;
    esac

    timeout="${LEKIWI_USB_ENUM_TIMEOUT:-20}"
    elapsed=0
    while [ "${elapsed}" -lt "${timeout}" ]; do
        # This flow needs the root hub, external hub, camera and Feetech CDC.
        set -- /dev/bus/usb/*/*
        if [ "$#" -ge 4 ] && [ -e "$4" ]; then
            if [ "${elapsed}" -gt 0 ]; then
                echo "USB devices ready after ${elapsed}s."
            fi
            return 0
        fi
        if [ "${elapsed}" -eq 0 ]; then
            echo "Waiting for Starry USB enumeration (timeout ${timeout}s)..."
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    echo "ERROR: Starry USB enumeration did not expose the hub, camera and Feetech CDC within ${timeout}s." >&2
    return 1
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
        echo "Build it from Linux first, then boot Starry with the same rootfs." >&2
        exit 1
    fi
else
    "${SCRIPT_DIR}/build_rk3588.sh" -b Release -l INFO
fi

wait_for_starry_usb

echo "=== LeKiwi full pick-and-bucket flow ==="
echo "  model       : ${MODEL_PATH}"
echo "  feetech_dev : ${FEETECH_DEV}"
echo "  uvc_index   : ${UVC_INDEX}"
echo "  rknn core   : ${RKNN_CORE_MASK:-012}"
echo "  ld path     : ${LD_LIBRARY_PATH:-}"
echo "  command     : ${SCRIPT_DIR}/build/tennis ${MODEL_PATH} ${FEETECH_DEV} ${UVC_INDEX} ${FEETECH_DEV} lekiwi"
echo ""

cd "${SCRIPT_DIR}" || exit 1
"${SCRIPT_DIR}/build/tennis" "${MODEL_PATH}" "${FEETECH_DEV}" "${UVC_INDEX}" "${FEETECH_DEV}" lekiwi
