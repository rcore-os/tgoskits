#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

usage() {
    cat <<'EOF'
Usage: demo-teacher.sh [--with-replay] [--no-docker]

Run the teacher-facing StarryOS K230 KPU demo.

Default:
  Run the native NNCase runtime path:
    .kmodel -> NNCase runtime -> 54 KPU commands -> /dev/kpu -> IRQ/done -> output tensor hash.

Options:
  --with-replay  Also run the optional full-sequence replay fallback if local capture assets exist.
  --no-docker    Do not auto-enter the starryos-dev Docker image.
  -h, --help     Show this help.
EOF
}

WITH_REPLAY=0
USE_DOCKER=${K230_TEACHER_DEMO_USE_DOCKER:-1}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-replay)
            WITH_REPLAY=1
            shift
            ;;
        --no-docker)
            USE_DOCKER=0
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

REPO_ROOT=$(find_repo_root "${SCRIPT_DIR}")
if [[ "${REPO_ROOT}" == */target/worktrees/* ]]; then
    BASE_ROOT="${REPO_ROOT%%/target/worktrees/*}"
else
    BASE_ROOT="${REPO_ROOT}"
fi

if [[ ! -f /.dockerenv && "${USE_DOCKER}" == "1" ]]; then
    if ! command -v docker >/dev/null 2>&1; then
        echo "docker is not available; run inside Docker or pass --no-docker" >&2
        exit 1
    fi

    IMAGE=${K230_TEACHER_DEMO_IMAGE:-starryos-dev:ubuntu-qemu10.2.1}
    CONTAINER_BASE=/workspace
    CONTAINER_REPO="${CONTAINER_BASE}${REPO_ROOT#"${BASE_ROOT}"}"
    CONTAINER_SCRIPT="${CONTAINER_REPO}/test-suit/starryos/k230-qemu/qemu-k230/demo-teacher.sh"

    DOCKER_CMD=(docker run --rm
        -e K230_TEACHER_DEMO_USE_DOCKER=0 \
        -v "${BASE_ROOT}:${CONTAINER_BASE}" \
        -w "${CONTAINER_REPO}" \
        "${IMAGE}" \
        bash "${CONTAINER_SCRIPT}")
    if [[ "${WITH_REPLAY}" == "1" ]]; then
        DOCKER_CMD+=(--with-replay)
    fi
    exec "${DOCKER_CMD[@]}"
fi

ensure_runtime_deps() {
    if command -v ldconfig >/dev/null 2>&1; then
        if ! ldconfig -p 2>/dev/null | grep -q 'libfdt\.so\.1'; then
            if command -v apt-get >/dev/null 2>&1; then
                echo "installing libfdt1 inside demo environment..."
                apt-get update >/dev/null
                apt-get install -y libfdt1 >/dev/null
            else
                echo "warning: libfdt.so.1 not found and apt-get is unavailable" >&2
            fi
        fi
    fi
}

section() {
    printf '\n==== %s ====\n' "$1"
}

run_case() {
    local case_name=$1
    local log_file=$2
    mkdir -p "$(dirname "${log_file}")"
    echo "running ${case_name}; streaming full output and saving log: ${log_file}"
    echo "this may take 1-2 minutes when caches are cold..."

    set +e
    if command -v stdbuf >/dev/null 2>&1; then
        stdbuf -oL -eL cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c "${case_name}" 2>&1 | tee "${log_file}"
    else
        cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c "${case_name}" 2>&1 | tee "${log_file}"
    fi
    local status=${PIPESTATUS[0]}
    set -e

    return "${status}"
}

print_runtime_evidence() {
    local log_file=$1
    grep -E \
        '^(NNCASE_MINIMAL: (load_model ok|model io|input\[0\]|output\[0\]|interp\.run done)|K230_SDK_COMPAT: (identity mmap l2|mirrored runtime rdata|gnne_enable run=54|stats)|YOLOV8N_DEMO: (decode|preprocess|output\[[0-3]\].*fnv1a64|output\[[0-3]\] stats|detections=|top )|NNCASE_MINIMAL_PASS|YOLOV8N_DEMO_PASS|K230_NNCASE_RUNTIME_PASS|all starry k230-qemu qemu tests passed)' \
        "${log_file}" || true
}

print_smoke_evidence() {
    local log_file=$1
    grep -E \
        'KPU_SMOKE: (info cfg=|run_wait_done|fake_output_zeroed|runtime_image .*status=|runtime_image_progress .*run=54/54|runtime_image .*full_sequence_delta.*runs=54|real_kmodel .*magic=LDMK)|KPU_SMOKE_PASS|all starry k230-qemu qemu tests passed|demo replay passed' \
        "${log_file}" || true
}

fail_with_log_tail() {
    local label=$1
    local status=$2
    local log_file=$3
    echo "${label} failed with status ${status}" >&2
    echo "last 120 log lines:" >&2
    tail -n 120 "${log_file}" >&2 || true
    exit "${status}"
}

LOG_DIR="${REPO_ROOT}/target/k230-kpu-demo"
RUNTIME_LOG="${LOG_DIR}/teacher-nncase-runtime.log"
REPLAY_LOG="${LOG_DIR}/yolov8n-full-sequence-replay.log"
CAPTURE="${SCRIPT_DIR}/kpu-smoke/c/assets/captures/yolov8n-full-sequence-delta.krun"

export PATH="${BASE_ROOT}/target/qemu-k230-docker-build:${BASE_ROOT}/target/qemu-k230-docker-build/bin:/opt/qemu-10.2.1/bin:/opt/riscv64-linux-musl-cross/bin:/opt/x86_64-linux-musl-cross/bin:${PATH}"

section "Demo Goal"
cat <<'EOF'
StarryOS on QEMU K230 can run the native KPU path:
  real yolov8n_320.kmodel
  -> official NNCase runtime inside StarryOS guest
  -> generated KPU command stream
  -> /dev/kpu submit
  -> IRQ/done
  -> nonzero output tensor hash/stats
EOF

section "Environment"
echo "repo=${REPO_ROOT}"
echo "qemu=$(command -v qemu-system-riscv64 || true)"
echo "log_dir=${LOG_DIR}"

ensure_runtime_deps

section "Native NNCase Runtime"
if run_case kpu-nncase-runtime "${RUNTIME_LOG}"; then
    print_runtime_evidence "${RUNTIME_LOG}"
else
    runtime_status=$?
    print_runtime_evidence "${RUNTIME_LOG}"
    fail_with_log_tail "kpu-nncase-runtime" "${runtime_status}" "${RUNTIME_LOG}"
fi

if ! grep -q 'K230_NNCASE_RUNTIME_PASS' "${RUNTIME_LOG}"; then
    print_runtime_evidence "${RUNTIME_LOG}"
    fail_with_log_tail "kpu-nncase-runtime" 1 "${RUNTIME_LOG}"
fi

section "Native Runtime Result"
cat <<'EOF'
PASS: StarryOS guest loaded the real .kmodel, NNCase generated KPU commands,
and QEMU KPU returned IRQ/done plus nonzero Starry output tensor hashes.
Known boundary: YOLO detection semantics are still being aligned with the official RT-Smart reference.
EOF

if [[ "${WITH_REPLAY}" == "1" ]]; then
    section "Full-Sequence Replay Fallback"
    if [[ ! -f "${CAPTURE}" ]]; then
        echo "skip replay: local capture asset is absent: ${CAPTURE}"
    else
        set +e
        if command -v stdbuf >/dev/null 2>&1; then
            K230_DEMO_USE_DOCKER=0 stdbuf -oL -eL bash "${SCRIPT_DIR}/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh" 2>&1 | tee "${REPLAY_LOG}"
        else
            K230_DEMO_USE_DOCKER=0 bash "${SCRIPT_DIR}/kpu-smoke/c/tools/demo-yolov8n-full-sequence-replay.sh" 2>&1 | tee "${REPLAY_LOG}"
        fi
        replay_status=${PIPESTATUS[0]}
        set -e
        print_smoke_evidence "${REPLAY_LOG}"
        if [[ "${replay_status}" -ne 0 ]]; then
            fail_with_log_tail "full-sequence replay" "${replay_status}" "${REPLAY_LOG}"
        fi
    fi
else
    section "Optional Replay"
    echo "Run with --with-replay to additionally show the kunOS/RT-Smart 54-command replay fallback."
fi

section "Final Demo Summary"
cat <<'EOF'
1. Bottom layer is complete: FDT probe, /dev/kpu, mmap/ioctl, command submit, IRQ/done.
2. Runtime layer is demonstrated: real .kmodel is loaded in StarryOS guest and NNCase generates 54 KPU commands.
3. Output evidence is checkable: nonzero tensor hashes/stats are printed.
4. Remaining work is semantic alignment of YOLO boxes/postprocess, not KPU device bring-up.
EOF
