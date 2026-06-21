#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/full_self_build.sh

or run stage 3 directly after the seed kernel and rootfs inputs already exist:

  KERNEL=target/aarch64-unknown-linux-musl/release/starryos.bin \
  ROOTFS=target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
  apps/starry/macos-selfbuild/run_selfbuild.sh

Stage 3 copies the managed rootfs to a per-run work image, prepares the app
overlay, injects it with `cargo xtask image inject`, launches QEMU/HVF, starts
the guest self-build command, and extracts the guest-built kernel from the work
image after QEMU exits.

Common knobs:
  SMP=4 JOBS=4 MEM=8192M SOURCE_TMPFS=1 QEMU_TIMEOUT_SEC=7200
  PREPARE_OVERLAY=1 ARTIFACT_EXTRACT=1
  QEMU_ACCEL=hvf QEMU_MACHINE=virt,gic-version=3 QEMU_CPU=host
  QEMU_APPEND='someboot.aarch64_gicd_spi=off'
  QEMU_NET=0
  QEMU_SNAPSHOT=0
  BOOT_ONLY=1
  EXTRA_RUSTFLAGS='<extra guest rustflags>'
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU AArch64 HVF" >&2
fi

find_tool() {
    local env_value="$1"
    local name="$2"
    local fallback="$3"

    if [[ -n "$env_value" ]]; then
        if command -v "$env_value" >/dev/null 2>&1; then
            command -v "$env_value"
            return
        fi
        if [[ -x "$env_value" ]]; then
            printf '%s\n' "$env_value"
            return
        fi
        echo "tool override is not executable or on PATH: $env_value" >&2
        exit 1
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

copy_image() {
    local src="$1"
    local dst="$2"

    rm -f "$dst"
    if cp -c "$src" "$dst" 2>/dev/null; then
        return
    fi
    if cp --reflink=auto "$src" "$dst" 2>/dev/null; then
        return
    fi
    cp "$src" "$dst"
}

qemu="$(find_tool "${QEMU:-}" qemu-system-aarch64 /opt/homebrew/bin/qemu-system-aarch64)"
debugfs="$(find_tool "${DEBUGFS:-}" debugfs /opt/homebrew/opt/e2fsprogs/sbin/debugfs)"
e2fsck="$(find_tool "${E2FSCK:-}" e2fsck /opt/homebrew/opt/e2fsprogs/sbin/e2fsck)"

git_value() {
    local fallback="$1"
    shift
    git -C "$repo_root" "$@" 2>/dev/null || printf '%s\n' "$fallback"
}

kernel="${KERNEL:-$repo_root/target/aarch64-unknown-linux-musl/release/starryos.bin}"
rootfs="${ROOTFS:-$repo_root/target/starry-macos-selfbuild/tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img}"
smp="${SMP:-4}"
jobs="${JOBS:-$smp}"
mem="${MEM:-8192M}"
qemu_accel="${QEMU_ACCEL:-hvf}"
qemu_machine="${QEMU_MACHINE:-virt,gic-version=3}"
qemu_cpu="${QEMU_CPU:-host}"
qemu_append="${QEMU_APPEND-someboot.aarch64_gicd_spi=off}"
qemu_net="${QEMU_NET:-0}"
boot_only="${BOOT_ONLY:-0}"
qemu_snapshot="${QEMU_SNAPSHOT:-0}"
prepare_overlay="${PREPARE_OVERLAY:-1}"
artifact_extract="${ARTIFACT_EXTRACT:-1}"
source_tmpfs="${SOURCE_TMPFS:-1}"
qemu_timeout_sec="${QEMU_TIMEOUT_SEC:-7200}"
stamp="${STAMP:-$(date +%Y%m%dT%H%M%S)}"
case_name="${CASE_NAME:-smp${smp}-j${jobs}}"
out_root="${OUT_ROOT:-$repo_root/target/starry-macos-selfbuild}"
work_rootfs="${WORK_ROOTFS:-$out_root/rootfs/rootfs-${case_name}-${stamp}.img}"
log="${LOG:-$out_root/logs/${case_name}-${stamp}.log}"
guest_script="$script_dir/guest-selfbuild.sh"
work_dir="$out_root/work/${case_name}-${stamp}"
overlay_dir="${STARRY_OVERLAY_DIR:-$work_dir/overlay}"
artifact_out_dir="${ARTIFACT_OUT_DIR:-$out_root/uploaded}"
guest_artifact_dir="${ARTIFACT_DIR:-/opt/starryos-selfbuild-artifacts}"
artifact_target="aarch64-unknown-none-softfloat"
artifact_bin_name="starryos"
failure_pattern='(panicked at|kernel panic|panic:|unhandled trap|trap frame|fatal exception|segmentation fault)'
require_fresh_rootfs="${REQUIRE_FRESH_ROOTFS:-1}"

rootfs_fsck() {
    local label="$1"
    local fsck_log="$work_dir/e2fsck-${label}.log"
    local fsck_rc

    set +e
    "$e2fsck" -fy "$work_rootfs" >"$fsck_log" 2>&1
    fsck_rc="$?"
    set -e

    if (( (fsck_rc & ~3) != 0 )); then
        cat "$fsck_log" >&2 || true
        echo "e2fsck failed for $work_rootfs (label=$label rc=$fsck_rc)" >&2
        return "$fsck_rc"
    fi
}

write_guest_runner() {
    local guest_runner="$1"

    mkdir -p "$(dirname "$guest_runner")"
    {
        printf '#!/bin/sh\n'
        printf 'set -eu\n'
        emit_export "JOBS" "$jobs"
        emit_export "SMP" "$smp"
        emit_export "RAYON_NUM_THREADS" "${RAYON_NUM_THREADS:-1}"
        emit_export "RUSTC_THREADS" "${RUSTC_THREADS:-2}"
        emit_export "SOURCE_TMPFS" "$source_tmpfs"
        emit_export "ARTIFACT_TO_BIN" "${ARTIFACT_TO_BIN:-1}"
        emit_export "STARRY_KALLSYMS_RESERVED" "${STARRY_KALLSYMS_RESERVED:-16M}"
        emit_export "FEATURES" "${FEATURES:-plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,smp}"
        emit_export "CARGO_BIN" "${CARGO_BIN:-/opt/cargo-nightly-sysroot}"
        emit_export "SOURCE_DIR" "${SOURCE_DIR:-/opt/tgoskits}"
        emit_export "WORK_DIR" "${WORK_DIR:-/tmp/starryos-selfbuild-src}"
        emit_export "CARGO_TARGET_DIR" "${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
        emit_export "ARTIFACT_DIR" "$guest_artifact_dir"
        emit_export "CARGO_VERBOSE" "${CARGO_VERBOSE:-0}"
        if [[ -n "${CARGO_PROFILE_RELEASE_LTO+x}" ]]; then
            emit_export "CARGO_PROFILE_RELEASE_LTO" "$CARGO_PROFILE_RELEASE_LTO"
        fi
        if [[ -n "${CARGO_PROFILE_RELEASE_OPT_LEVEL+x}" ]]; then
            emit_export "CARGO_PROFILE_RELEASE_OPT_LEVEL" "$CARGO_PROFILE_RELEASE_OPT_LEVEL"
        fi
        if [[ -n "${CARGO_PROFILE_RELEASE_CODEGEN_UNITS+x}" ]]; then
            emit_export "CARGO_PROFILE_RELEASE_CODEGEN_UNITS" "$CARGO_PROFILE_RELEASE_CODEGEN_UNITS"
        fi
        if [[ -n "${CARGO_PROFILE_RELEASE_DEBUG+x}" ]]; then
            emit_export "CARGO_PROFILE_RELEASE_DEBUG" "$CARGO_PROFILE_RELEASE_DEBUG"
        fi
        emit_export "TGOSKITS_COMMIT" "$source_commit"
        emit_export "TGOSKITS_REF" "$source_ref"
        if [[ -n "${LINK_RUSTFLAGS+x}" ]]; then
            emit_export "LINK_RUSTFLAGS" "$LINK_RUSTFLAGS"
        fi
        if [[ -n "${EXTRA_RUSTFLAGS:-}" ]]; then
            emit_export "EXTRA_RUSTFLAGS" "$EXTRA_RUSTFLAGS"
        fi
        printf 'exec /bin/sh /opt/starry-macos-selfbuild.sh\n'
    } >"$guest_runner"
    chmod 0755 "$guest_runner"
}

inject_overlay_with_xtask() {
    local inject_log="$work_dir/xtask-image-inject.log"

    echo "inject_overlay=$overlay_dir"
    if ! (cd "$repo_root" && cargo xtask image inject "$work_rootfs" --overlay "$overlay_dir") >"$inject_log" 2>&1; then
        cat "$inject_log" >&2 || true
        echo "failed to inject overlay into $work_rootfs" >&2
        return 1
    fi
    LC_ALL=C grep -a " injected into " "$inject_log" | tail -1 \
        || echo "overlay injected into $work_rootfs"
}

prepare_and_inject_overlay() {
    [[ "$prepare_overlay" = "1" ]] || return 0

    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir"
    STARRY_APP_DIR="$script_dir" \
        STARRY_WORKSPACE="$repo_root" \
        STARRY_OVERLAY_DIR="$overlay_dir" \
        "$script_dir/prebuild.sh"
    install -m 0755 "$guest_script" "$overlay_dir/opt/starry-macos-selfbuild.sh"
    write_guest_runner "$overlay_dir/opt/starry-macos-run.sh"
    inject_overlay_with_xtask
}

if [[ ! -f "$kernel" ]]; then
    echo "kernel not found: $kernel" >&2
    exit 1
fi

if [[ ! -f "$rootfs" ]]; then
    echo "rootfs not found: $rootfs" >&2
    exit 1
fi

actual_commit="$(git_value unknown rev-parse HEAD)"
if [[ -n "${TGOSKITS_COMMIT:-}" && "$actual_commit" != "unknown" && "$TGOSKITS_COMMIT" != "$actual_commit" ]]; then
    echo "TGOSKITS_COMMIT=$TGOSKITS_COMMIT does not match workspace HEAD $actual_commit" >&2
    exit 1
fi
source_commit="${TGOSKITS_COMMIT:-$actual_commit}"
source_ref="${TGOSKITS_REF:-$(git_value detached symbolic-ref --quiet --short HEAD)}"

mkdir -p "$(dirname "$work_rootfs")" "$(dirname "$log")" "$work_dir"
copy_image "$rootfs" "$work_rootfs"
prepare_and_inject_overlay

if [[ "$require_fresh_rootfs" = "1" ]]; then
    rootfs_meta="$("$debugfs" -R "cat /opt/tgoskits-src.meta" "$work_rootfs" 2>/dev/null || true)"
    rootfs_commit="$(printf '%s\n' "$rootfs_meta" | sed -n 's/^commit=//p' | tail -1)"
    if [[ -z "$rootfs_commit" ]]; then
        cat >&2 <<EOF
rootfs source metadata is missing in $rootfs.
Rebuild or refresh the self-build rootfs from the current checkout:

  apps/starry/macos-selfbuild/full_self_build.sh
EOF
        exit 1
    fi
    if [[ "$actual_commit" != "unknown" && "$rootfs_commit" != "$source_commit" ]]; then
        cat >&2 <<EOF
rootfs source commit does not match this checkout.
  checkout: $source_commit
  rootfs:   $rootfs_commit

This usually means an old rootfs is being reused. Refresh it before running:

  apps/starry/macos-selfbuild/full_self_build.sh

Set REQUIRE_FRESH_ROOTFS=0 only for deliberate stale-rootfs experiments.
EOF
        exit 1
    fi
fi

rootfs_fsck pre-qemu

input_fifo="$work_dir/qemu-stdin.fifo"
mkfifo "$input_fifo"

echo "log=$log"
echo "kernel=$kernel"
echo "rootfs_copy=$work_rootfs"
echo "artifact_out_dir=$artifact_out_dir"
echo "qemu=$qemu"
echo "qemu_accel=$qemu_accel qemu_machine=$qemu_machine qemu_cpu=$qemu_cpu qemu_net=$qemu_net qemu_append=$qemu_append"
echo "smp=$smp jobs=$jobs mem=$mem source_tmpfs=$source_tmpfs boot_only=$boot_only qemu_snapshot=$qemu_snapshot prepare_overlay=$prepare_overlay artifact_extract=$artifact_extract qemu_timeout_sec=$qemu_timeout_sec"
echo "source_commit=$source_commit source_ref=$source_ref"
: >"$log"

qemu_args=()
if [[ "$qemu_snapshot" = "1" ]]; then
    qemu_args+=(-snapshot)
fi
qemu_args+=(
    -nographic
    -accel "$qemu_accel"
    -machine "$qemu_machine"
    -cpu "$qemu_cpu"
    -m "$mem"
    -smp "$smp"
    -device virtio-blk-pci,drive=disk0
    -drive "id=disk0,if=none,format=raw,file=$work_rootfs,file.locking=off"
    -kernel "$kernel"
    -monitor none
    -serial mon:stdio
)
if [[ -n "$qemu_append" ]]; then
    qemu_args+=(-append "$qemu_append")
fi

if [[ "$qemu_net" != "0" ]]; then
    qemu_args+=(
        -device virtio-net-pci,netdev=net0
        -netdev user,id=net0
    )
else
    qemu_args+=(-net none)
fi

"$qemu" \
    "${qemu_args[@]}" \
    <"$input_fifo" >"$log" 2>&1 &
qemu_pid="$!"

exec 3>"$input_fifo"
sent_cmd=0
host_rc=124
start_seconds="$SECONDS"
heartbeat_sec="${HOST_HEARTBEAT_SEC:-30}"
next_heartbeat="$heartbeat_sec"
set +e
while kill -0 "$qemu_pid" 2>/dev/null; do
    elapsed=$((SECONDS - start_seconds))
    if [[ "$sent_cmd" = "0" ]] && LC_ALL=C grep -a -q "root@starry:" "$log"; then
        if [[ "$boot_only" = "1" ]]; then
            echo "===HOST-QEMU-STOP reason=boot-only-shell pid=$qemu_pid===" >>"$log"
            kill "$qemu_pid" 2>/dev/null || true
            wait "$qemu_pid" 2>/dev/null
            host_rc=0
            break
        else
            printf '/bin/sh /opt/starry-macos-run.sh\n' >&3
            echo "===HOST-SENT-SELFBUILD-COMMAND===" >>"$log"
            sent_cmd=1
        fi
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

    if LC_ALL=C grep -a -E -i -q "$failure_pattern" "$log"; then
        echo "===HOST-QEMU-STOP reason=failure-pattern pid=$qemu_pid===" >>"$log"
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null
        host_rc=1
        break
    fi

    if (( heartbeat_sec > 0 && elapsed >= next_heartbeat )); then
        heartbeat_line="$(
            LC_ALL=C tr '\r' '\n' <"$log" \
                | grep -a -E '===STARRY-MACOS-SELFBUILD|Building \[|Compiling|Finished|error:' \
                | tail -1 \
                | cut -c 1-220
        )"
        echo "host-heartbeat elapsed=${elapsed}s qemu_pid=$qemu_pid ${heartbeat_line:-waiting-for-guest-output}"
        next_heartbeat=$((elapsed + heartbeat_sec))
    fi

    if [[ "$qemu_timeout_sec" != "0" ]]; then
        if (( elapsed >= qemu_timeout_sec )); then
            echo "===HOST-QEMU-STOP reason=timeout pid=$qemu_pid elapsed=$elapsed timeout=$qemu_timeout_sec===" >>"$log"
            kill "$qemu_pid" 2>/dev/null || true
            wait "$qemu_pid" 2>/dev/null
            host_rc=124
            break
        fi
    fi

    sleep 2
done

if ! kill -0 "$qemu_pid" 2>/dev/null && ! LC_ALL=C grep -a -q "===HOST-QEMU-STOP" "$log"; then
    wait "$qemu_pid" 2>/dev/null
    qemu_rc="$?"
    if [[ "$boot_only" = "1" ]] && LC_ALL=C grep -a -q "root@starry:" "$log"; then
        echo "===HOST-QEMU-STOP reason=boot-only-shell pid=$qemu_pid rc=$qemu_rc===" >>"$log"
        host_rc=0
    elif [[ "$boot_only" = "1" ]]; then
        echo "===HOST-QEMU-STOP reason=qemu-exit-without-shell pid=$qemu_pid rc=$qemu_rc===" >>"$log"
        if [[ "$qemu_rc" = "0" ]]; then
            host_rc=1
        else
            host_rc="$qemu_rc"
        fi
    elif [[ "$sent_cmd" = "1" ]] \
        && ! LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-RUN-END rc=" "$log"; then
        echo "===HOST-QEMU-STOP reason=qemu-exit-without-run-end pid=$qemu_pid rc=$qemu_rc===" >>"$log"
        host_rc="$qemu_rc"
    else
        echo "===HOST-QEMU-STOP reason=qemu-exit pid=$qemu_pid rc=$qemu_rc===" >>"$log"
        host_rc="$qemu_rc"
    fi
fi

if LC_ALL=C grep -a -E -i -q "$failure_pattern" "$log"; then
    host_rc=1
fi

if LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-RUN-END rc=" "$log"; then
    marker_rc="$(
        LC_ALL=C sed -n 's/^===STARRY-MACOS-SELFBUILD-RUN-END rc=\([0-9][0-9]*\)===.*/\1/p' "$log" | tail -1
    )"
    host_rc="${marker_rc:-$host_rc}"
fi

if [[ "$boot_only" != "1" && "$sent_cmd" = "1" ]] \
    && ! LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-RUN-END rc=" "$log"; then
    host_rc=1
fi

if kill -0 "$qemu_pid" 2>/dev/null; then
    if ! LC_ALL=C grep -a -q "===HOST-QEMU-STOP" "$log"; then
        echo "===HOST-QEMU-STOP reason=host-cleanup pid=$qemu_pid rc=$host_rc===" >>"$log"
    fi
    kill "$qemu_pid" 2>/dev/null || true
    wait "$qemu_pid" 2>/dev/null
fi
set -e

exec 3>&-

dump_guest_artifact() {
    local guest_path="$1"
    local host_path="$2"
    local dump_log="$work_dir/debugfs-dump.log"

    rm -f "$host_path"
    if ! "$debugfs" -R "dump -p $guest_path $host_path" "$work_rootfs" >"$dump_log" 2>&1; then
        cat "$dump_log" >&2 || true
        echo "failed to extract $guest_path from $work_rootfs" >&2
        return 1
    fi
    if [[ ! -s "$host_path" ]]; then
        echo "extracted artifact is empty or missing: $host_path" >&2
        return 1
    fi
}

extract_rootfs_artifacts() {
    local stem="$artifact_bin_name-$artifact_target"
    local guest_elf="${guest_artifact_dir%/}/$stem"
    local host_elf="$artifact_out_dir/$stem"
    local guest_bin="$guest_elf.bin"
    local host_bin="$host_elf.bin"

    [[ "$boot_only" != "1" ]] || return 0
    [[ "$artifact_extract" = "1" ]] || return 0

    mkdir -p "$artifact_out_dir"
    rootfs_fsck post-qemu
    dump_guest_artifact "$guest_elf" "$host_elf"
    echo "extracted_kernel_elf=$host_elf"

    if [[ "${ARTIFACT_TO_BIN:-1}" = "1" ]]; then
        dump_guest_artifact "$guest_bin" "$host_bin"
        echo "extracted_kernel_bin=$host_bin"
    fi
}

if [[ "$host_rc" = "0" ]]; then
    if ! extract_rootfs_artifacts; then
        host_rc=1
    fi
fi

if LC_ALL=C grep -a -q "===STARRY-MACOS-SELFBUILD-PASS" "$log"; then
    LC_ALL=C grep -a "===STARRY-MACOS-SELFBUILD-PASS" "$log" | tail -1
fi

exit "$host_rc"
