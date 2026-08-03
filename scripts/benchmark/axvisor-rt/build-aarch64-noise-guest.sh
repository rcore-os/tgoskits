#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
source_file=$script_dir/guest/aarch64_rt_noise.S
output=$workspace/tmp/axvisor-rt/aarch64-rt-noise.bin
duration_seconds=180

usage() {
    cat <<'EOF'
usage: build-aarch64-noise-guest.sh [options]

Options:
  --output PATH          Flat AArch64 guest image output
  --duration-seconds N   Bounded CPU workload duration (default: 180)
EOF
}

while (($# > 0)); do
    case "$1" in
        --output) output=${2:?--output requires a value}; shift 2 ;;
        --duration-seconds) duration_seconds=${2:?--duration-seconds requires a value}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ ! "$duration_seconds" =~ ^[0-9]+$ ]] || \
    ((duration_seconds < 1 || duration_seconds > 3600)); then
    echo "--duration-seconds must be an integer in 1..3600: $duration_seconds" >&2
    exit 2
fi
if [[ "$output" != /* ]]; then
    output=$workspace/$output
fi
if [[ ! -r "$source_file" ]]; then
    echo "noise guest source is not readable: $source_file" >&2
    exit 1
fi

compiler=${CC_aarch64_unknown_linux_musl:-aarch64-linux-musl-cc}
if command -v llvm-objcopy >/dev/null 2>&1; then
    objcopy=llvm-objcopy
elif command -v aarch64-linux-musl-objcopy >/dev/null 2>&1; then
    objcopy=aarch64-linux-musl-objcopy
else
    echo "llvm-objcopy or aarch64-linux-musl-objcopy is required" >&2
    exit 1
fi
for command_name in "$compiler" "$objcopy" file readelf sha256sum wc; do
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "required noise guest build tool not found: $command_name" >&2
        exit 1
    }
done

mkdir -p "$(dirname -- "$output")"
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/axvisor-rt-noise.XXXXXX")
object=$build_dir/noise.o
elf=$build_dir/noise.elf
cleanup() {
    rm -f -- "$object" "$elf"
    rmdir -- "$build_dir" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

"$compiler" \
    -c -x assembler-with-cpp -march=armv8-a \
    -DAXVISOR_RT_NOISE_SECONDS="$duration_seconds" \
    -DAXVISOR_RT_NOISE_COUNTER_HZ=24000000 \
    -o "$object" "$source_file"
"$compiler" \
    -nostdlib -static \
    -Wl,--build-id=none \
    -Wl,-Ttext=0x40000000 \
    -Wl,-e,_start \
    -Wl,-z,max-page-size=4096 \
    -o "$elf" "$object"
"$objcopy" -O binary "$elf" "$output"

entry=$(readelf -h "$elf" | awk '/Entry point address:/ { print $4 }')
if [[ "$entry" != 0x40000000 ]]; then
    echo "noise guest entry point mismatch: $entry" >&2
    exit 1
fi
image_bytes=$(wc -c <"$output")
if ((image_bytes == 0 || image_bytes > 4096)); then
    echo "noise guest flat image has an unexpected size: $image_bytes" >&2
    exit 1
fi

file "$elf"
sha256sum "$source_file" "$output"
echo "AXVISOR_RT_NOISE_GUEST_READY path=$output entry=$entry bytes=$image_bytes duration_seconds=$duration_seconds counter_hz=24000000"
