#!/bin/sh
# One bounded robot hardware flow. The fixture must have its chassis raised.
# A tennis ball is optional: when none is detected, the application still
# verifies camera/NPU throughput and exercises controlled wheel/arm motion.
# A failed first process is retried once to tolerate the known first-open
# Feetech CDC timeout.

set -u

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
MIN_FPS="${1:-12.5}"
MODEL_PATH="${MODEL_PATH:-${SCRIPT_DIR}/models/tennis.rknn}"
FEETECH_DEV="${FEETECH_DEV:-auto}"
UVC_INDEX="${UVC_INDEX:-0}"

if [ ! -x "${SCRIPT_DIR}/build/tennis" ]; then
    echo "[ROBOT_CI] RESULT=FAIL reason=missing_binary path=${SCRIPT_DIR}/build/tennis"
    exit 1
fi
if [ ! -f "${MODEL_PATH}" ]; then
    echo "[ROBOT_CI] RESULT=FAIL reason=missing_model path=${MODEL_PATH}"
    exit 1
fi

RKNN_CORE_MASK="${RKNN_CORE_MASK:-0}"
AKA_STATE_LOG_INTERVAL_MS="${AKA_STATE_LOG_INTERVAL_MS:-3000}"
ROBOT_CI_MIN_FPS="${MIN_FPS}"
export RKNN_CORE_MASK AKA_STATE_LOG_INTERVAL_MS
export ROBOT_CI_MIN_FPS

wait_for_usb() {
    timeout="${LEKIWI_USB_ENUM_TIMEOUT:-20}"
    elapsed=0
    checked_ids=0
    camera_ready=0
    feetech_ready=0
    while [ "${elapsed}" -lt "${timeout}" ]; do
        if command -v lsusb >/dev/null 2>&1; then
            checked_ids=1
            camera_ready=0
            feetech_ready=0
            lsusb -d 0ac8:0346 2>/dev/null | grep -q . && camera_ready=1
            lsusb -d 1a86:55d3 2>/dev/null | grep -q . && feetech_ready=1
            if [ "${camera_ready}" -eq 1 ] && [ "${feetech_ready}" -eq 1 ]; then
                echo "[ROBOT_CI] USB_READY camera=0ac8:0346 feetech=1a86:55d3 elapsed_s=${elapsed}"
                return 0
            fi
            sleep 1
            elapsed=$((elapsed + 1))
            continue
        fi

        set -- /dev/bus/usb/*/*
        # Starry images without lsusb fall back to node-count readiness. The
        # application still verifies the exact camera and Feetech devices.
        if [ "$#" -ge 4 ] && [ -e "$4" ]; then
            echo "[ROBOT_CI] USB_READY devices=$# check=node-count elapsed_s=${elapsed}"
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    if [ "${checked_ids}" -eq 1 ]; then
        echo "[ROBOT_CI] RESULT=FAIL reason=usb_device_missing camera=${camera_ready} feetech=${feetech_ready} elapsed_s=${elapsed}"
        return 1
    fi
    echo "[ROBOT_CI] RESULT=FAIL reason=usb_enumeration_timeout elapsed_s=${elapsed}"
    return 1
}

run_attempt() {
    attempt="$1"
    echo "[ROBOT_CI] ATTEMPT_BEGIN index=${attempt}/2"
    cd "${SCRIPT_DIR}" || return 1
    "${SCRIPT_DIR}/build/tennis" \
        "${MODEL_PATH}" "${FEETECH_DEV}" "${UVC_INDEX}" \
        "${FEETECH_DEV}" lekiwi --robot-ci-once
}

wait_for_usb || exit 1

if run_attempt 1; then
    echo "[ROBOT_CI] RESULT=PASS attempts=1"
    exit 0
fi

echo "[ROBOT_CI] RETRY reason=first_attempt_failed delay_s=1"
sleep 1

if run_attempt 2; then
    echo "[ROBOT_CI] RESULT=PASS attempts=2"
    exit 0
fi

echo "[ROBOT_CI] RESULT=FAIL attempts=2"
exit 1
