#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
  ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
  apps/starry/macos-selfbuild/run_selfbuild.sh

Common knobs:
  SMP=8 JOBS=8 SOURCE_TMPFS=1 QEMU_TIMEOUT_SEC=7200
  EXTRA_RUSTFLAGS='<extra guest rustflags>'
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU HVF" >&2
fi

find_tool() {
    local env_value="$1"
    local name="$2"
    local fallback="$3"

    if [[ -n "$env_value" ]]; then
        printf '%s\n' "$env_value"
        return
    fi
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return
    fi
    if [[ -n "$fallback" && -x "$fallback" ]]; then
        printf '%s\n' "$fallback"
        return
    fi
    echo "$name not found; install it or set the matching environment variable" >&2
    exit 1
}

shell_quote() {
    local value="$1"
    local i char
    printf "'"
    for ((i = 0; i < ${#value}; i++)); do
        char="${value:i:1}"
        if [[ "$char" == "'" ]]; then
            printf '%s' "'\\''"
        else
            printf '%s' "$char"
        fi
    done
    printf "'"
}

emit_export() {
    local name="$1"
    local value="$2"
    printf 'export %s=' "$name"
    shell_quote "$value"
    printf '\n'
}

qemu="$(find_tool "${QEMU:-}" qemu-system-aarch64 /opt/homebrew/bin/qemu-system-aarch64)"
debugfs="$(find_tool "${DEBUGFS:-}" debugfs /opt/homebrew/opt/e2fsprogs/sbin/debugfs)"

kernel="${KERNEL:-$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin}"
rootfs="${ROOTFS:-$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img}"
smp="${SMP:-8}"
jobs="${JOBS:-$smp}"
mem="${MEM:-4096M}"
source_tmpfs="${SOURCE_TMPFS:-1}"
qemu_timeout_sec="${QEMU_TIMEOUT_SEC:-7200}"
stamp="${STAMP:-$(date +%Y%m%dT%H%M%S)}"
case_name="${CASE_NAME:-smp${smp}-j${jobs}}"
out_root="${OUT_ROOT:-$repo_root/target/starry-macos-selfbuild}"
work_rootfs="${WORK_ROOTFS:-$out_root/rootfs/rootfs-${case_name}-${stamp}.img}"
log="${LOG:-$out_root/logs/${case_name}-${stamp}.log}"
guest_script="$script_dir/guest-selfbuild.sh"
work_dir="$out_root/work/${case_name}-${stamp}"

if [[ ! -f "$kernel" ]]; then
    echo "kernel not found: $kernel" >&2
    exit 1
fi

if [[ ! -f "$rootfs" ]]; then
    echo "rootfs not found: $rootfs" >&2
    exit 1
fi

mkdir -p "$(dirname "$work_rootfs")" "$(dirname "$log")" "$work_dir"
cp "$rootfs" "$work_rootfs"

guest_runner="$work_dir/starry-macos-run.sh"
{
    printf '#!/bin/sh\n'
    printf 'set -eu\n'
    emit_export "JOBS" "$jobs"
    emit_export "SMP" "$smp"
    emit_export "SOURCE_TMPFS" "$source_tmpfs"
    emit_export "PROFILE" "${PROFILE:-release}"
    emit_export "BUILD_TARGET" "${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
    emit_export "BUILD_PACKAGE" "${BUILD_PACKAGE:-starryos}"
    emit_export "BUILD_BIN" "${BUILD_BIN:-starryos}"
    emit_export "BUILD_STD" "${BUILD_STD:-core,alloc,compiler_builtins}"
    emit_export "FEATURES" "${FEATURES:-qemu,gic-v3,cntv-timer,smp}"
    emit_export "NO_DEFAULT_FEATURES" "${NO_DEFAULT_FEATURES:-0}"
    emit_export "CARGO_SUBCOMMAND" "${CARGO_SUBCOMMAND:-build}"
    emit_export "SOURCE_DIR" "${SOURCE_DIR:-/opt/tgoskits}"
    emit_export "WORK_DIR" "${WORK_DIR:-/tmp/starryos-selfbuild-src}"
    emit_export "CARGO_TARGET_DIR" "${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
    emit_export "CARGO_PROFILE_RELEASE_LTO" "${CARGO_PROFILE_RELEASE_LTO:-false}"
    emit_export "CARGO_PROFILE_RELEASE_OPT_LEVEL" "${CARGO_PROFILE_RELEASE_OPT_LEVEL:-0}"
    emit_export "CARGO_PROFILE_RELEASE_CODEGEN_UNITS" "${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-256}"
    emit_export "CARGO_PROFILE_RELEASE_DEBUG" "${CARGO_PROFILE_RELEASE_DEBUG:-0}"
    if [[ -n "${EXTRA_RUSTFLAGS:-}" ]]; then
        emit_export "EXTRA_RUSTFLAGS" "$EXTRA_RUSTFLAGS"
    fi
    printf 'exec /bin/sh /opt/starry-macos-selfbuild.sh\n'
} >"$guest_runner"
chmod +x "$guest_runner"

debugfs_cmd="$work_dir/debugfs-inject.cmd"
cat >"$debugfs_cmd" <<EOF
rm /opt/starry-macos-selfbuild.sh
rm /opt/starry-macos-run.sh
write $guest_script /opt/starry-macos-selfbuild.sh
write $guest_runner /opt/starry-macos-run.sh
sif /opt/starry-macos-selfbuild.sh mode 0100755
sif /opt/starry-macos-run.sh mode 0100755
EOF

"$debugfs" -w -f "$debugfs_cmd" "$work_rootfs" >/dev/null

input_fifo="$work_dir/qemu-stdin.fifo"
mkfifo "$input_fifo"

echo "log=$log"
echo "kernel=$kernel"
echo "rootfs_copy=$work_rootfs"
echo "qemu=$qemu"
echo "smp=$smp jobs=$jobs mem=$mem source_tmpfs=$source_tmpfs qemu_timeout_sec=$qemu_timeout_sec"
: >"$log"

"$qemu" \
    -snapshot \
    -nographic \
    -accel hvf \
    -machine virt,gic-version=3 \
    -cpu host \
    -m "$mem" \
    -smp "$smp" \
    -device virtio-blk-pci,drive=disk0 \
    -drive "id=disk0,if=none,format=raw,file=$work_rootfs,file.locking=off" \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -kernel "$kernel" \
    -monitor none \
    -serial mon:stdio \
    <"$input_fifo" >"$log" 2>&1 &
qemu_pid="$!"

exec 3>"$input_fifo"
sent_cmd=0
host_rc=124
start_ts="$(date +%s)"

set +e
while kill -0 "$qemu_pid" 2>/dev/null; do
    if [[ "$sent_cmd" = "0" ]] && LC_ALL=C grep -a -q "root@starry:" "$log"; then
        printf '/bin/sh /opt/starry-macos-run.sh\n' >&3
        echo "===HOST-SENT-SELFBUILD-COMMAND===" >>"$log"
        sent_cmd=1
    fi

    if LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-RUN-END rc=" "$log"; then
        marker_rc="$(
            LC_ALL=C sed -n 's/^===STARRY-MACOS-SELFBUILD-RUN-END rc=\([0-9][0-9]*\)===.*/\1/p' "$log" | tail -1
        )"
        echo "===HOST-QEMU-STOP reason=guest-run-end pid=$qemu_pid rc=${marker_rc:-unknown}===" >>"$log"
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null
        host_rc="${marker_rc:-0}"
        break
    fi

    if LC_ALL=C grep -a -E -i -q '(panic|panicked at|unhandled trap|trap frame|fatal|segmentation fault)' "$log"; then
        echo "===HOST-QEMU-STOP reason=failure-pattern pid=$qemu_pid===" >>"$log"
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null
        host_rc=1
        break
    fi

    if [[ "$qemu_timeout_sec" != "0" ]]; then
        now_ts="$(date +%s)"
        if (( now_ts - start_ts >= qemu_timeout_sec )); then
            echo "===HOST-QEMU-STOP reason=timeout pid=$qemu_pid elapsed=$((now_ts - start_ts)) timeout=$qemu_timeout_sec===" >>"$log"
            kill "$qemu_pid" 2>/dev/null || true
            wait "$qemu_pid" 2>/dev/null
            host_rc=124
            break
        fi
    fi

    sleep 2
done

if kill -0 "$qemu_pid" 2>/dev/null; then
    kill "$qemu_pid" 2>/dev/null || true
    wait "$qemu_pid" 2>/dev/null
fi
set -e

exec 3>&-

if LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-PASS" "$log"; then
    LC_ALL=C grep -a "===STARRY-MACOS-SELFBUILD-PASS" "$log" | tail -1
fi

exit "$host_rc"
