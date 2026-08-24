#!/usr/bin/env bash
set -euo pipefail

# Judge-facing entrypoint for the Task 1-3 QEMU acceptance scenarios.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
evidence_root="${TASK123_EVIDENCE_DIR:-$repo_root/tmp/competition-task123/evidence}"
ncnn_revision="946fe3fb14a8dff8c06df763f67be522167b2f00"
zephyr_revision="dccb09599635bdff17633fa7e9dab014b91dce90"

usage() {
    cat <<'EOF'
Usage:
  scripts/competition/task123.sh doctor
  scripts/competition/task123.sh --list
  scripts/competition/task123.sh build [quick|full]
  scripts/competition/task123.sh run SCENARIO
  scripts/competition/task123.sh suite [quick|acceptance|full|video]

Run `scripts/competition/task123.sh --list` for scenario and suite names.
Generated evidence is written below tmp/competition-task123/evidence by default.
EOF
}

list_scenarios() {
    cat <<'EOF'
Scenarios:
  ci-contracts             Task 2/3 Rust and Python contract/regression gate
  task3-yolo-smoke         Fresh AArch64 ncnn binary performs real YOLO inference
  task1-scheduler-ab       Same dual-Guest load under RR and FP-RR schedulers
  task23-normal            YOLO -> CONTROL -> RTOS STATUS/ACK, with two pcaps
  task2-drop-ack           One lost ACK, retransmission and duplicate suppression
  task2-retry-exhausted    Bounded retries, Safe state and recovery
  task2-blackout           Link blackout, Safe state and post-blackout recovery
  task2-out-of-order       Reject an out-of-order CONTROL frame and recover
  task2-invalid-parameter  Reject an invalid control value and recover
  task3-model-rejected     Invalid model output enters the defined Safe path

Suites:
  quick       ci-contracts + task3-yolo-smoke
  acceptance  task1-scheduler-ab + normal + blackout + model-rejected
  full        all ten scenarios
  video       short evidence order used by the recommended recording script
EOF
}

configure_cross_tools() {
    if [[ -n "${CROSS_ROOT:-}" ]]; then
        case ":$PATH:" in
            *":$CROSS_ROOT/bin:"*) ;;
            *) export PATH="$CROSS_ROOT/bin:$PATH" ;;
        esac
        export CROSS_CC="${CROSS_CC:-$CROSS_ROOT/bin/aarch64-linux-musl-gcc}"
        export CROSS_CXX="${CROSS_CXX:-$CROSS_ROOT/bin/aarch64-linux-musl-g++}"
        export CROSS_AR="${CROSS_AR:-$CROSS_ROOT/bin/aarch64-linux-musl-ar}"
        export CROSS_RANLIB="${CROSS_RANLIB:-$CROSS_ROOT/bin/aarch64-linux-musl-ranlib}"
        export CROSS_COMPILE="${CROSS_COMPILE:-$CROSS_ROOT/bin/aarch64-linux-musl-}"
    fi
}

configured_tool() {
    local override_name="$1" command_name="$2" configured
    configured="${!override_name:-}"
    if [[ -n "$configured" ]]; then
        [[ -x "$configured" ]] && printf '%s\n' "$configured"
        return
    fi
    command -v "$command_name" 2>/dev/null || true
}

find_ncnn_source() {
    local candidate
    for candidate in \
        "${NCNN_SOURCE:-}" \
        "$repo_root/tmp/task3-yolo/ncnn-source" \
        "$repo_root/tmp/task3-yolo/ncnn-source-fresh" \
        "$repo_root/tmp/competition-task123/downloads/ncnn"; do
        if [[ -n "$candidate" && -f "$candidate/CMakeLists.txt" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
}

find_pnnx() {
    local candidate
    for candidate in \
        "${PNNX:-}" \
        "$(command -v pnnx 2>/dev/null || true)" \
        "$repo_root/tmp/task3-yolo/pnnx-tool/20260526/pnnx-20260526-linux/pnnx" \
        "$repo_root/tmp/competition-task123/downloads/pnnx-20260526-linux/pnnx"; do
        if [[ -n "$candidate" && -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
}

find_zephyr_base() {
    local candidate
    for candidate in \
        "${ZEPHYR_BASE:-}" \
        "$repo_root/tmp/competition-task123/downloads/zephyr-$zephyr_revision"; do
        if [[ -n "$candidate" && -f "$candidate/CMakeLists.txt" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
}

doctor() {
    configure_cross_tools
    local failures=0 tool
    local commands=(
        git cargo rustup python3 cmake ninja qemu-system-aarch64 qemu-aarch64
        debugfs e2fsck lsof sha256sum realpath dtc
    )
    printf 'Task 1-3 environment check\n'
    printf '  repository: %s\n' "$repo_root"
    printf '  commit:     %s\n' "$(git -C "$repo_root" rev-parse HEAD)"
    for tool in "${commands[@]}"; do
        if command -v "$tool" >/dev/null 2>&1; then
            printf '  [OK]      %s\n' "$tool"
        else
            printf '  [MISSING] %s\n' "$tool"
            failures=$((failures + 1))
        fi
    done

    local override command_name resolved
    while read -r override command_name; do
        resolved="$(configured_tool "$override" "$command_name")"
        if [[ -n "$resolved" ]]; then
            printf '  [OK]      %s (%s)\n' "$command_name" "$resolved"
        else
            printf '  [MISSING] %s (set %s or add it to PATH)\n' "$command_name" "$override"
            failures=$((failures + 1))
        fi
    done <<'EOF'
CROSS_CC aarch64-linux-musl-gcc
CROSS_CXX aarch64-linux-musl-g++
CROSS_AR aarch64-linux-musl-ar
CROSS_RANLIB aarch64-linux-musl-ranlib
EOF

    if python3 -c 'from PIL import Image' >/dev/null 2>&1; then
        printf '  [OK]      Python Pillow\n'
    else
        printf '  [MISSING] Python Pillow\n'
        failures=$((failures + 1))
    fi

    local ncnn_source pnnx zephyr_base onnx
    ncnn_source="$(find_ncnn_source || true)"
    pnnx="$(find_pnnx || true)"
    zephyr_base="$(find_zephyr_base || true)"
    onnx="${YOLO_ONNX:-$repo_root/tmp/task3-yolo/yolo11n.onnx}"
    if [[ -n "$ncnn_source" ]] &&
        [[ "$(git -C "$ncnn_source" rev-parse HEAD 2>/dev/null || true)" == "$ncnn_revision" ]]; then
        printf '  [OK]      ncnn source %s\n' "$ncnn_revision"
    else
        printf '  [MISSING] pinned ncnn source; set NCNN_SOURCE (commit %s)\n' "$ncnn_revision"
        failures=$((failures + 1))
    fi
    if [[ -n "$pnnx" ]]; then
        printf '  [OK]      pnnx 20260526 candidate (%s)\n' "$pnnx"
    else
        printf '  [MISSING] pnnx 20260526; set PNNX\n'
        failures=$((failures + 1))
    fi
    if [[ -n "$zephyr_base" ]] &&
        [[ "$(git -C "$zephyr_base" rev-parse HEAD 2>/dev/null || true)" == "$zephyr_revision" ]]; then
        printf '  [OK]      Zephyr source %s\n' "$zephyr_revision"
    else
        printf '  [MISSING] pinned Zephyr source; set ZEPHYR_BASE (commit %s)\n' "$zephyr_revision"
        failures=$((failures + 1))
    fi
    if [[ -s "$onnx" ]] &&
        [[ "$(sha256sum "$onnx" | awk '{print $1}')" == \
            "634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b" ]]; then
        printf '  [OK]      pinned YOLO ONNX\n'
    else
        printf '  [MISSING] YOLO_ONNX with SHA256 634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b\n'
        failures=$((failures + 1))
    fi

    if ((failures > 0)); then
        cat <<'EOF'

Install common Ubuntu dependencies with:
  sudo apt-get update
  sudo apt-get install build-essential cmake ninja-build qemu-system-arm qemu-user \
    e2fsprogs lsof device-tree-compiler python3 python3-pil git curl xz-utils

The AArch64 musl cross compiler is not Ubuntu's native musl-tools package.
Install an aarch64-linux-musl toolchain, then either add its bin directory to
PATH or export CROSS_ROOT=/path/to/aarch64-linux-musl-cross.

Downloaded sources may be reused. Compiled outputs are removed and rebuilt by
`task123.sh build full`. See scripts/competition/README-task123.md for the
pinned source setup commands.
EOF
        return 1
    fi
    printf '\nDOCTOR_PASS\n'
}

fresh_output_dir() {
    local directory="$1"
    case "$directory" in
        "$repo_root/tmp/task3-yolo/"*|\
        "$repo_root/tmp/net-dual-guest/"*|\
        "$repo_root/tmp/starry-task1-periodic") ;;
        *) printf 'error: refusing to clear unexpected output path: %s\n' "$directory" >&2; return 1 ;;
    esac
    rm -rf -- "$directory"
    mkdir -p "$directory"
}

fresh_release_dir() {
    local directory="$1"
    case "$directory" in
        "$repo_root/target/aarch64-unknown-none-softfloat/release"|\
        "$repo_root/target/aarch64-unknown-linux-musl/release") ;;
        *) printf 'error: refusing to clear unexpected release path: %s\n' "$directory" >&2; return 1 ;;
    esac
    rm -rf -- "$directory"
}

build_quick() {
    (cd "$repo_root" && bash scripts/test/net-dual-guest/run-ci-regression.sh)
}

build_full() {
    doctor
    configure_cross_tools
    local ncnn_source pnnx zephyr_base onnx
    ncnn_source="$(find_ncnn_source)"
    pnnx="$(find_pnnx)"
    zephyr_base="$(find_zephyr_base)"
    onnx="${YOLO_ONNX:-$repo_root/tmp/task3-yolo/yolo11n.onnx}"

    # These images are built through nested/custom Cargo workspaces, for which
    # `cargo clean -p ...` at the repository root can report "Removed 0 files".
    # Remove the exact release roots instead so acceptance never reuses an old
    # StarryOS or AxVisor image. Downloaded sources and toolchains stay intact.
    fresh_release_dir "$repo_root/target/aarch64-unknown-none-softfloat/release"
    fresh_release_dir "$repo_root/target/aarch64-unknown-linux-musl/release"
    rm -rf -- "$repo_root/target/starryos-task2-rust"
    build_quick
    fresh_output_dir "$repo_root/tmp/task3-yolo/ncnn-aarch64"
    NCNN_SOURCE="$ncnn_source" \
        "$repo_root/scripts/task3/build-ncnn-aarch64.sh"

    fresh_output_dir "$repo_root/tmp/task3-yolo/ncnn-model"
    YOLO_ONNX="$onnx" PNNX="$pnnx" \
        "$repo_root/scripts/task3/convert-yolo-ncnn.sh"
    "$repo_root/scripts/task3/prepare-yolo-ncnn-input.sh"
    "$repo_root/scripts/task3/prepare-yolo-ncnn-ab-inputs.sh"

    fresh_output_dir "$repo_root/tmp/task3-yolo/ncnn-smoke"
    "$repo_root/scripts/task3/run-ncnn-smoke.sh"

    local variant fault
    while read -r variant fault; do
        fresh_output_dir "$repo_root/tmp/net-dual-guest/zephyr-task2-starry-$variant"
        ZEPHYR_BASE="$zephyr_base" TASK2_ZEPHYR_VIRTIO_SLOT=0 \
            TASK2_FAULT_MODE="$fault" \
            OUT_DIR="$repo_root/tmp/net-dual-guest/zephyr-task2-starry-$variant" \
            BUILD_DIR="$repo_root/tmp/net-dual-guest/zephyr-task2-starry-$variant/cargo-target" \
            "$repo_root/scripts/test/net-dual-guest/build-zephyr-task2.sh"
    done <<'EOF'
normal none
drop-ack drop-ack-once
retry-exhausted drop-ack-always
EOF

    # The checked-in baseline VM config consumes the canonical Zephyr path.
    # Install this build's normal variant there; do not depend on an artifact
    # left by an earlier checkout or validation run.
    fresh_output_dir "$repo_root/tmp/net-dual-guest/zephyr-task2"
    cp "$repo_root/tmp/net-dual-guest/zephyr-task2-starry-normal/zephyr-task2.bin" \
        "$repo_root/tmp/net-dual-guest/zephyr-task2/zephyr-task2.bin"
    cp "$repo_root/tmp/net-dual-guest/zephyr-task2-starry-normal/manifest.toml" \
        "$repo_root/tmp/net-dual-guest/zephyr-task2/manifest.toml"

    fresh_output_dir "$repo_root/tmp/starry-task1-periodic"
    ZEPHYR_BASE="$zephyr_base" \
        OUT_DIR="$repo_root/tmp/starry-task1-periodic" \
        BUILD_DIR="$repo_root/tmp/starry-task1-periodic/cargo-target" \
        "$repo_root/scripts/test/rt-partition/build-zephyr-periodic.sh"

    (cd "$repo_root" && cargo xtask starry rootfs --arch aarch64)
    (cd "$repo_root" && cargo xtask starry app qemu \
        --test-case starryos-task2 --arch aarch64 \
        --qemu-config scripts/competition/qemu-aarch64-starry-build-smoke.toml)
    local starry_elf rootfs
    starry_elf="$repo_root/target/aarch64-unknown-none-softfloat/release/starryos"
    rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
    if ! grep -aFq 'registered virtio network device' "$starry_elf"; then
        printf 'error: freshly built StarryOS image does not contain the virtio-net driver\n' >&2
        return 1
    fi
    # App staging updates ext4 through debugfs. Finish its metadata repair now,
    # before any scenario makes a disposable copy of the image.
    local fsck_status=0
    e2fsck -fy "$rootfs" || fsck_status=$?
    if ((fsck_status > 1)); then
        printf 'error: failed to repair freshly staged rootfs (e2fsck=%d)\n' \
            "$fsck_status" >&2
        return 1
    fi
    e2fsck -fn "$rootfs"
    local build_vm_dir build_rtos_vm
    build_vm_dir="$repo_root/tmp/competition-task123/build"
    build_rtos_vm="$build_vm_dir/vm-aarch64-p2-switch-rtos.runtime.toml"
    mkdir -p "$build_vm_dir"
    python3 "$repo_root/scripts/test/net-dual-guest/render_vm_entry.py" \
        "$repo_root/tmp/net-dual-guest/zephyr-task2/manifest.toml" \
        "$repo_root/scripts/test/net-dual-guest/vm-aarch64-p2-switch-rtos.toml" \
        "$build_rtos_vm"
    (cd "$repo_root" && cargo xtask axvisor build \
        --config scripts/test/net-dual-guest/axvisor-qemu-debug.toml \
        --vmconfigs scripts/test/net-dual-guest/vm-aarch64-starry-switch.toml \
        --vmconfigs "$build_rtos_vm")
    printf 'TASK123_FULL_BUILD_PASS\n'
}

new_evidence_path() {
    local label="$1" stamp
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    printf '%s/%s-%s-%s\n' "$evidence_root" "$stamp" "$label" "$$"
}

run_scenario() {
    local scenario="$1" output_dir="${2:-}"
    if [[ -z "$output_dir" ]]; then
        output_dir="$(new_evidence_path "$scenario")"
    fi
    mkdir -p "$(dirname "$output_dir")"
    printf 'TASK123_SCENARIO_START name=%s evidence=%s\n' "$scenario" "$output_dir"
    case "$scenario" in
        ci-contracts)
            mkdir -p "$output_dir"
            (cd "$repo_root" && bash scripts/test/net-dual-guest/run-ci-regression.sh) \
                2>&1 | tee "$output_dir/run.log"
            ;;
        task3-yolo-smoke)
            mkdir -p "$output_dir"
            OUT_DIR="$output_dir/build" \
                "$repo_root/scripts/task3/run-ncnn-smoke.sh" \
                2>&1 | tee "$output_dir/run.log"
            ;;
        task1-scheduler-ab)
            "$repo_root/scripts/test/net-dual-guest/run-starry-task1-periodic-ab.sh" "$output_dir"
            ;;
        task23-normal|task2-drop-ack|task2-retry-exhausted|task2-blackout|task2-out-of-order|task2-invalid-parameter|task3-model-rejected)
            local internal_scenario="${scenario#task2-}"
            internal_scenario="${internal_scenario#task3-}"
            [[ "$scenario" == task23-normal ]] && internal_scenario="normal"
            "$repo_root/scripts/test/net-dual-guest/run-starry-task23-scenario.sh" \
                "$internal_scenario" "$output_dir"
            ;;
        *)
            printf 'error: unknown scenario: %s\n' "$scenario" >&2
            list_scenarios >&2
            return 2
            ;;
    esac
    git -C "$repo_root" rev-parse HEAD > "$output_dir/git-head.txt"
    printf 'TASK123_SCENARIO_PASS name=%s evidence=%s\n' "$scenario" "$output_dir"
}

run_suite() {
    local suite="$1" suite_dir scenario
    local -a scenarios
    case "$suite" in
        quick) scenarios=(ci-contracts task3-yolo-smoke) ;;
        acceptance|video)
            scenarios=(task1-scheduler-ab task23-normal task2-blackout task3-model-rejected)
            ;;
        full)
            scenarios=(
                ci-contracts task3-yolo-smoke task1-scheduler-ab task23-normal
                task2-drop-ack task2-retry-exhausted task2-blackout
                task2-out-of-order task2-invalid-parameter task3-model-rejected
            )
            ;;
        *) printf 'error: unknown suite: %s\n' "$suite" >&2; return 2 ;;
    esac
    suite_dir="$(new_evidence_path "suite-$suite")"
    mkdir -p "$suite_dir"
    for scenario in "${scenarios[@]}"; do
        run_scenario "$scenario" "$suite_dir/$scenario"
    done
    printf 'TASK123_SUITE_PASS name=%s evidence=%s\n' "$suite" "$suite_dir"
}

main() {
    local command="${1:-}"
    case "$command" in
        doctor) [[ $# -eq 1 ]] || { usage >&2; return 2; }; doctor ;;
        --list|list) [[ $# -eq 1 ]] || { usage >&2; return 2; }; list_scenarios ;;
        build)
            [[ $# -le 2 ]] || { usage >&2; return 2; }
            case "${2:-full}" in
                quick) build_quick ;;
                full) build_full ;;
                *) usage >&2; return 2 ;;
            esac
            ;;
        run) [[ $# -eq 2 ]] || { usage >&2; return 2; }; run_scenario "$2" ;;
        suite) [[ $# -eq 2 ]] || { usage >&2; return 2; }; run_suite "$2" ;;
        -h|--help) usage ;;
        *) usage >&2; return 2 ;;
    esac
}

main "$@"
