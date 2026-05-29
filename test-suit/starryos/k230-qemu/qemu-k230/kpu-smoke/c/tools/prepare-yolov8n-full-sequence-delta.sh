#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
C_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

find_repo_root() {
    local dir=$1
    while [[ "${dir}" != "/" ]]; do
        if [[ -f "${dir}/Cargo.toml" && -d "${dir}/test-suit" ]]; then
            printf '%s\n' "${dir}"
            return 0
        fi
        dir=$(dirname "${dir}")
    done
    return 1
}

REPO_ROOT=$(find_repo_root "${C_DIR}")

if [[ "${REPO_ROOT}" == */target/worktrees/* ]]; then
    BASE_ROOT="${REPO_ROOT%%/target/worktrees/*}"
else
    BASE_ROOT="${REPO_ROOT}"
fi

OFFICIAL_DIR=${K230_OFFICIAL_DIR:-"${BASE_ROOT}/target/official-k230"}
SNAPSHOT_DIR=${K230_KPU_CAPTURE_DIR_OUT:-"${OFFICIAL_DIR}/yolov8n-prestart-snapshots"}
TRACE=${K230_KPU_FULL_TRACE:-"${OFFICIAL_DIR}/kunos-yolov8n-full-series-kpu-trace.log"}
CAPTURE_DIR=${K230_KPU_CAPTURE_ASSET_DIR:-"${C_DIR}/assets/captures"}
PYTHON=${PYTHON:-python3}
RUN_CAPTURE=0

usage() {
    cat <<EOF
Usage: $0 [--capture]

Generates assets/captures/yolov8n-full-sequence-delta.krun from the kunOS
RT-Smart YOLOv8n full-series KPU capture.

Options:
  --capture   First run QEMU official reference capture with K230_KPU_CAPTURE_DIR.

Environment:
  K230_OFFICIAL_DIR              defaults to ${OFFICIAL_DIR}
  K230_KPU_CAPTURE_DIR_OUT       defaults to ${SNAPSHOT_DIR}
  K230_KPU_FULL_TRACE            defaults to ${TRACE}
  K230_KPU_CAPTURE_ASSET_DIR     defaults to ${CAPTURE_DIR}
  K230_QEMU                      optional qemu-system-riscv64 override
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --capture)
            RUN_CAPTURE=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "${RUN_CAPTURE}" -eq 1 ]]; then
    "${PYTHON}" "${SCRIPT_DIR}/capture-kunos-yolov8n-full-series.py" \
        --repo-root "${REPO_ROOT}" \
        --base-root "${BASE_ROOT}" \
        --out-dir "${OFFICIAL_DIR}" \
        --capture-dir "${SNAPSHOT_DIR}" \
        --trace "${TRACE}"
fi

if [[ ! -f "${TRACE}" ]]; then
    echo "missing full-series trace: ${TRACE}" >&2
    echo "run with --capture, or set K230_KPU_FULL_TRACE to an existing trace" >&2
    exit 1
fi

if [[ ! -f "${SNAPSHOT_DIR}/run-0001-low16m.bin" ]]; then
    echo "missing pre-start snapshots under ${SNAPSHOT_DIR}" >&2
    echo "run with --capture, or set K230_KPU_CAPTURE_DIR_OUT to an existing snapshot directory" >&2
    exit 1
fi

mkdir -p "${CAPTURE_DIR}"

"${PYTHON}" "${SCRIPT_DIR}/make-yolov8n-last-command-capture.py" \
    --mode full-sequence-delta \
    --trace "${TRACE}" \
    --snapshot-dir "${SNAPSHOT_DIR}" \
    --out-dir "${CAPTURE_DIR}" \
    --copy-snapshots

echo "prepared ${CAPTURE_DIR}/yolov8n-full-sequence-delta.krun"
echo "next: bash ${SCRIPT_DIR}/demo-yolov8n-full-sequence-replay.sh"
