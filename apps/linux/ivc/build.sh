#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
arch="${AXVISOR_IVC_ARCH:-aarch64}"
out_dir="${AXVISOR_IVC_OUT_DIR:-${script_dir}/build/out-${arch}}"

usage() {
    cat <<'USAGE'
Usage:
  AXVISOR_IVC_ARCH=aarch64 \
  AXVISOR_IVC_OUT_DIR=/path/to/out \
  apps/linux/ivc/build.sh

  apps/linux/ivc/build.sh --test

Builds the Linux-side Axvisor IVC user-space test programs.
The kernel module is built by tgosimages and is expected to already exist in
the target rootfs as /root/axvisor.ko.
Outputs:
  <out>/ivc-publish
  <out>/ivc-subscribe

The --test mode builds and runs host-side unit tests with ${CC:-cc}.
USAGE
}

pick_cross_prefix() {
    local preferred="$1"
    local fallback="$2"
    local native="${3:-__none__}"

    if command -v "${preferred}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${preferred}"
    elif command -v "${fallback}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${fallback}"
    elif [[ "${native}" != "__none__" ]] && command -v "${native}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${native}"
    else
        echo "no usable compiler found: tried ${preferred}gcc, ${fallback}gcc and ${native}gcc" >&2
        return 1
    fi
}

case "${1:-}" in
    -h|--help|help)
        usage
        exit 0
        ;;
    ""|--test)
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

mkdir -p "${out_dir}"

build_user_program() {
    local name="$1"
    local source="$2"

    "${demo_cross}gcc" \
        -I"${script_dir}/include" \
        -Wall -Wextra -Os -s -Wl,--gc-sections -static \
        -o "${out_dir}/${name}" \
        "${source}" \
        "${script_dir}/lib/ivc.c"
}

run_host_tests() {
    local test_out_dir="${AXVISOR_IVC_TEST_OUT_DIR:-${out_dir}/host-tests}"
    local host_cc="${CC:-cc}"

    mkdir -p "${test_out_dir}"
    "${host_cc}" \
        -I"${script_dir}/include" \
        -Wall -Wextra -Werror -O0 -g -Wl,--wrap=write \
        -o "${test_out_dir}/ivc-write-all-zero-progress" \
        "${script_dir}/tests/ivc_write_all_zero_progress.c" \
        "${script_dir}/lib/ivc.c"
    "${test_out_dir}/ivc-write-all-zero-progress"

    "${host_cc}" \
        -I"${script_dir}/include" \
        -Wall -Wextra -Werror -O0 -g -Wl,--wrap=open -Wl,--wrap=ioctl \
        -o "${test_out_dir}/ivc-open-failure-rolls-back" \
        "${script_dir}/tests/ivc_open_failure_rolls_back.c" \
        "${script_dir}/lib/ivc.c"
    "${test_out_dir}/ivc-open-failure-rolls-back"

    echo "AXVISOR_IVC_TEST_OUT_DIR=${test_out_dir}"
}

if [[ "${1:-}" == "--test" ]]; then
    run_host_tests
    exit 0
fi

case "${arch}" in
    aarch64)
        demo_cross="$(pick_cross_prefix "${AARCH64_MUSL_CROSS:-aarch64-linux-musl-}" "${AARCH64_CROSS_COMPILE:-aarch64-linux-gnu-}")"
        ;;
    x86_64)
        demo_cross="$(pick_cross_prefix "${X86_64_MUSL_CROSS:-x86_64-linux-musl-}" "${X86_64_GNU_CROSS:-x86_64-linux-gnu-}" "")"
        ;;
    *)
        echo "unsupported AXVISOR_IVC_ARCH: ${arch}" >&2
        exit 2
        ;;
esac

build_user_program "ivc-publish" "${script_dir}/publisher/main.c"
build_user_program "ivc-subscribe" "${script_dir}/subscriber/main.c"

echo "AXVISOR_IVC_OUT_DIR=${out_dir}"
