#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
compiler=${CC:-aarch64-linux-musl-gcc}
source_file=$script_dir/guest/axvisor_rt_probe.c
output=$workspace/tmp/axvisor-rt/axvisor-rt-probe

usage() {
    cat <<'EOF'
usage: build-probe.sh [--cc COMPILER] [--output PATH]

Build the static AArch64 realtime probe from the tracked C source. The output
defaults to tmp/axvisor-rt/axvisor-rt-probe.
EOF
}

while (($# > 0)); do
    case "$1" in
        --cc) compiler=${2:?--cc requires a value}; shift 2 ;;
        --output) output=${2:?--output requires a value}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if ! command -v "$compiler" >/dev/null 2>&1; then
    echo "AArch64 static compiler not found: $compiler" >&2
    exit 1
fi
if [[ ! -r "$source_file" ]]; then
    echo "Probe source is not readable: $source_file" >&2
    exit 1
fi
if [[ "$output" != /* ]]; then
    output=$workspace/$output
fi
if [[ "$output" == *[[:space:]]* ]]; then
    echo "Probe output path must not contain whitespace: $output" >&2
    exit 1
fi
for command_name in file grep mktemp mv sed sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required probe build tool not found: $command_name" >&2
        exit 1
    fi
done

mkdir -p -- "$(dirname -- "$output")"
temporary=$(mktemp "$(dirname -- "$output")/.axvisor-rt-probe.XXXXXX")
cleanup() {
    rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

"$compiler" -std=c11 -O2 -Wall -Wextra -Werror -static -pthread \
    "$source_file" -o "$temporary"
if ! file "$temporary" | grep -Eq 'ELF 64-bit .* ARM aarch64.*statically linked'; then
    echo "Compiler did not produce a static AArch64 ELF executable" >&2
    exit 1
fi
mv -f -- "$temporary" "$output"
temporary=
trap - EXIT HUP INT TERM

"$compiler" --version | sed -n '1p'
sha256sum "$source_file" "$output"
echo "AXVISOR_RT_PROBE_READY path=$output"
