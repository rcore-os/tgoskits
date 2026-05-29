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

CONTAINER=${K230_DEMO_DOCKER_CONTAINER:-k230-official-runtime}
USE_DOCKER=${K230_DEMO_USE_DOCKER:-1}

host_to_container_repo() {
    if [[ -n "${K230_DEMO_CONTAINER_REPO:-}" ]]; then
        printf '%s\n' "${K230_DEMO_CONTAINER_REPO}"
    elif [[ "${REPO_ROOT}" == /Users/joshua/tmp/tgoskits* ]]; then
        printf '/mnt/tgoskits%s\n' "${REPO_ROOT#/Users/joshua/tmp/tgoskits}"
    else
        printf '%s\n' "${REPO_ROOT}"
    fi
}

if [[ ! -f /.dockerenv && "${USE_DOCKER}" == "1" ]]; then
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is not available; run this script inside Docker or set K230_DEMO_USE_DOCKER=0" >&2
        exit 1
    fi
    if docker ps --format '{{.Names}}' | grep -qx "${CONTAINER}"; then
        CONTAINER_REPO=$(host_to_container_repo)
        exec docker exec "${CONTAINER}" bash -lc \
            "cd '${CONTAINER_REPO}' && K230_DEMO_USE_DOCKER=0 bash '${CONTAINER_REPO}/test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh'"
    else
        echo "docker container ${CONTAINER} is not running or docker is not accessible" >&2
        echo "start it, run inside Docker, or set K230_DEMO_USE_DOCKER=0 to force host execution" >&2
        exit 1
    fi
fi

CAPTURE_DIR="${C_DIR}/assets/captures"
CAPTURE="${CAPTURE_DIR}/yolov8n-full-sequence-delta.krun"
MODEL="${C_DIR}/assets/kmodels/yolov8n_320.kmodel"
LOG_DIR="${REPO_ROOT}/target/k230-kpu-demo"
LOG="${LOG_DIR}/yolov8n-full-sequence-replay.log"

if [[ ! -f "${CAPTURE}" ]]; then
    echo "missing ${CAPTURE}" >&2
    echo "prepare it with:" >&2
    echo "  bash ${SCRIPT_DIR}/prepare-yolov8n-full-sequence-delta.sh" >&2
    exit 1
fi

if [[ ! -f "${MODEL}" ]]; then
    echo "warning: ${MODEL} is absent; smoke still replays .krun, but real_kmodel evidence will be skipped" >&2
    echo "install it with: bash ${SCRIPT_DIR}/prepare-real-kmodel.sh" >&2
fi

mkdir -p "${LOG_DIR}"

export PATH="${BASE_ROOT}/target/qemu-k230-docker-build:/opt/riscv64-linux-musl-cross/bin:${PATH}"
echo "repo=${REPO_ROOT}"
echo "qemu=$(command -v qemu-system-riscv64 || true)"
echo "capture=${CAPTURE}"
echo "log=${LOG}"
echo
echo "running StarryOS K230 KPU YOLOv8n full-sequence replay..."

set +e
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke 2>&1 | tee "${LOG}"
status=${PIPESTATUS[0]}
set -e

echo
echo "demo evidence:"
grep -E 'KPU_SMOKE: (optional_runtime_image selecting full_sequence_delta|runtime_image_progress .*run=54/54|runtime_image .*full_sequence_delta.*runs=54|real_kmodel .*magic=LDMK)|KPU_SMOKE_PASS' "${LOG}" || true

if [[ "${status}" -ne 0 ]]; then
    echo "demo failed with status ${status}; see ${LOG}" >&2
    exit "${status}"
fi

if ! grep -q 'KPU_SMOKE_PASS' "${LOG}"; then
    echo "demo did not produce KPU_SMOKE_PASS; see ${LOG}" >&2
    exit 1
fi

echo "demo replay passed"
