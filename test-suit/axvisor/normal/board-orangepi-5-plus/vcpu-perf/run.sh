#!/usr/bin/env bash
set -euo pipefail
suite=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd "$suite/../../../../.." && pwd)
cd "$workspace"
baseline=${VCPU_PERF_BASELINE:-"$suite/baseline.toml"}
verdict_args=()
board_args=()
for arg in "$@"; do
    if [[ "$arg" == "--measure" ]]; then
        verdict_args+=("--measure")
    else
        board_args+=("$arg")
    fi
done
mkdir -p tmp/vcpu-perf
cargo xtask arceos build -p arceos-vcpu-perf -c "$suite/guest-build.toml"
objcopy="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-objcopy"
"$objcopy" --strip-all -O binary target/aarch64-unknown-linux-musl/release/arceos-vcpu-perf target/aarch64-unknown-linux-musl/release/arceos-vcpu-perf.bin
cargo xtask axvisor test board --board orangepi-5-plus-vcpu-perf "${board_args[@]}" 2>&1 | tee tmp/vcpu-perf/run.log
python3 "$suite/check.py" tmp/vcpu-perf/run.log "$baseline" "${verdict_args[@]}"
