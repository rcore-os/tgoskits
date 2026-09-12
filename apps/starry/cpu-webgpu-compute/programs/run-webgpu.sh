#!/bin/sh
# On-target WebGPU JS/TS carpet runner for StarryOS.
#
# WebGPU standalone runtime = Deno (V8 + a built-in copy of gfx-rs wgpu-core, the Rust WebGPU engine
# Firefox and Servo also use, and the exact engine cpu-wgpu-compute #1576 builds on musl). Deno runs
# .js and .ts natively; its global navigator.gpu drives wgpu-core -> the Vulkan backend -> Mesa
# lavapipe (software Vulkan on the CPU), so the carpets run entirely on the CPU, no GPU. Alpine ships a
# native-musl Deno for x86_64 and aarch64, so both arches run this carpet on-target.
#
# The gate is manifest-honest: prebuild.sh writes expected_cells listing exactly the cells whose
# runtime it provisioned on this arch. This runner passes only when every listed cell prints its
# "<MARKER> OK <n>" marker (fail==0 && total==EXPECTED==pass, EXPECTED>=2). It never emits a 0-carpet
# TEST PASSED.
set -u
APP=/opt/cpu-webgpu-compute
CARPETS="$APP/carpets"
MANIFEST="$APP/expected_cells"

VK_ICD="${VK_ICD:-/usr/share/vulkan/icd.d/lvp_icd.json}"
export VK_DRIVER_FILES="$VK_ICD"
export VK_ICD_FILENAMES="$VK_ICD"
mkdir -p /tmp/vkrt; export XDG_RUNTIME_DIR=/tmp/vkrt
mkdir -p /tmp/deno; export DENO_DIR=/tmp/deno
# Single-thread the llvmpipe/lavapipe rasterizer: StarryOS runs one vCPU and the carpets assert
# numerical correctness, not throughput, so thread count does not change the pass counts.
export LP_NUM_THREADS=1

if [ ! -s "$MANIFEST" ]; then
    echo "cpu-webgpu-compute: no WebGPU runtime provisioned on $(uname -m) (manifest empty)"
    echo "cpu-webgpu-compute: standalone Deno WebGPU is x86_64-only on musl; full JS/TS WebGPU on this"
    echo "cpu-webgpu-compute: arch is the browser path (#391). Refusing to emit a 0-carpet pass."
    echo "TEST FAILED"
    exit 1
fi

DENO="$(command -v deno 2>/dev/null || echo /usr/bin/deno)"
if [ ! -x "$DENO" ]; then
    echo "cpu-webgpu-compute: manifest lists cells but deno is missing - provisioning broken"
    echo "TEST FAILED"
    exit 1
fi

EXPECTED=$(grep -c . "$MANIFEST")
pass=0; fail=0
# run <cell> <marker> <file> - only runs cells the manifest advertises; a cell passes when its output
# carries "<marker> OK <n>".
run() {
    cell="$1"; marker="$2"; file="$3"
    grep -qx "$cell" "$MANIFEST" || return 0
    out="$("$DENO" run --no-check --unstable-webgpu --allow-all "$file" 2>&1)"
    if echo "$out" | grep -qE "^$marker OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|^$marker OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $cell"
        fail=$((fail + 1))
    fi
}

run webgpu_js WEBGPU_JS_FULL_API "$CARPETS/webgpu_js/webgpu_js_full_api.js"
run webgpu_ts WEBGPU_TS_FULL_API "$CARPETS/webgpu_ts/webgpu_ts_full_api.ts"

total=$((pass + fail))
echo "cpu-webgpu-compute: $pass/$total WebGPU cells OK on $(uname -m) via Deno (wgpu-core) + lavapipe"
if [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ] && [ "$EXPECTED" -ge 2 ]; then
    echo "TEST PASSED"
else
    echo "TEST FAILED"
fi
