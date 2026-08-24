#!/usr/bin/env bash
set -euo pipefail

# Rebuild the existing Task1 Linux version with tracing enabled while keeping
# its scheduling and full-dynticks policy unchanged. This image is diagnostic
# input only; use RT_LINUX_KERNEL_OVERRIDE to select it explicitly.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
linux_source="${LINUX_SOURCE:-}"
base_image="${BASE_LINUX_IMAGE:-${repo_root}/tmp/rt-partition/linux-qemu}"
out_dir="${OUT_DIR:-${repo_root}/tmp/rt-partition/linux-trace}"
cross_prefix="${CROSS_COMPILE:-aarch64-linux-gnu-}"
jobs="${JOBS:-$(nproc)}"

[[ -n "$linux_source" ]] || {
    printf 'error: set LINUX_SOURCE to a Linux source tree matching the base Image\n' >&2
    exit 2
}
for path in "$linux_source/Makefile" "$linux_source/scripts/extract-ikconfig" \
    "$linux_source/scripts/config" "$base_image"; do
    [[ -e "$path" ]] || { printf 'error: required input is missing: %s\n' "$path" >&2; exit 1; }
done
command -v "${cross_prefix}gcc" >/dev/null 2>&1 || {
    printf 'error: cross compiler is not available: %sgcc\n' "$cross_prefix" >&2
    exit 1
}
[[ "$jobs" =~ ^[0-9]+$ ]] && (( jobs > 0 )) || {
    printf 'error: JOBS must be a positive integer\n' >&2
    exit 2
}

mkdir -p "$out_dir"
base_config="$out_dir/base.config"
build_dir="$out_dir/build"
final_config="$out_dir/trace.config"
image="$out_dir/linux-qemu-trace"
manifest="$out_dir/trace-kernel.manifest"

"$linux_source/scripts/extract-ikconfig" "$base_image" > "$base_config"
[[ -s "$base_config" ]] || {
    printf 'error: base Linux Image does not contain an embedded config\n' >&2
    exit 1
}
mkdir -p "$build_dir"
cp "$base_config" "$build_dir/.config"

config="$linux_source/scripts/config"
"$config" --file "$build_dir/.config" \
    --set-str LOCALVERSION "-task1-trace" \
    --disable LOCALVERSION_AUTO \
    --enable FTRACE \
    --enable FUNCTION_TRACER \
    --enable FUNCTION_GRAPH_TRACER \
    --enable SCHED_TRACER \
    --enable IRQSOFF_TRACER \
    --enable PREEMPT_TRACER \
    --enable TRACER_SNAPSHOT \
    --enable HIST_TRIGGERS \
    --enable OSNOISE_TRACER \
    --enable TIMERLAT_TRACER

make -C "$linux_source" O="$build_dir" ARCH=arm64 CROSS_COMPILE="$cross_prefix" olddefconfig
cp "$build_dir/.config" "$final_config"

config_value() {
    local file="$1"
    local option="$2"
    sed -n -e "s/^CONFIG_${option}=//p" -e "s/^# CONFIG_${option} is not set$/n/p" "$file" | tail -n 1
}

assert_config_unchanged() {
    local option="$1"
    local before after
    before="$(config_value "$base_config" "$option")"
    after="$(config_value "$final_config" "$option")"
    [[ "$before" == "$after" ]] || {
        printf 'error: tracing build changed CONFIG_%s from %s to %s\n' \
            "$option" "$before" "$after" >&2
        exit 1
    }
}

assert_config_enabled() {
    local option="$1"
    [[ "$(config_value "$final_config" "$option")" == "y" ]] || {
        printf 'error: CONFIG_%s was not enabled by olddefconfig\n' "$option" >&2
        exit 1
    }
}

assert_config_unchanged PREEMPT
assert_config_unchanged NO_HZ_FULL
assert_config_unchanged SHADOW_CALL_STACK
assert_config_unchanged INIT_STACK_ALL_PATTERN
assert_config_unchanged INIT_STACK_ALL_ZERO
assert_config_unchanged INIT_STACK_NONE
assert_config_enabled FTRACE
assert_config_enabled OSNOISE_TRACER
assert_config_enabled TIMERLAT_TRACER

make -C "$linux_source" O="$build_dir" ARCH=arm64 CROSS_COMPILE="$cross_prefix" -j"$jobs" Image
cp "$build_dir/arch/arm64/boot/Image" "$image"

source_version="$(make -s -C "$linux_source" kernelversion)"
{
    printf 'source=%s\n' "$linux_source"
    printf 'source_version=%s\n' "$source_version"
    printf 'base_image=%s\n' "$base_image"
    printf 'base_sha256=%s\n' "$(sha256sum "$base_image" | awk '{print $1}')"
    printf 'image=%s\n' "$image"
    printf 'image_sha256=%s\n' "$(sha256sum "$image" | awk '{print $1}')"
    printf 'cross_compile=%s\n' "$cross_prefix"
    printf 'compiler=%s\n' "$("${cross_prefix}gcc" --version | head -n 1)"
    printf 'config_preempt=%s\n' "$(config_value "$final_config" PREEMPT)"
    printf 'config_no_hz_full=%s\n' "$(config_value "$final_config" NO_HZ_FULL)"
    printf 'config_ftrace=%s\n' "$(config_value "$final_config" FTRACE)"
    printf 'config_osnoise_tracer=%s\n' "$(config_value "$final_config" OSNOISE_TRACER)"
    printf 'config_timerlat_tracer=%s\n' "$(config_value "$final_config" TIMERLAT_TRACER)"
} > "$manifest"

"$linux_source/scripts/diffconfig" "$base_config" "$final_config" > "$out_dir/config.diff"
printf 'trace_linux_image=%s manifest=%s\n' "$image" "$manifest"
