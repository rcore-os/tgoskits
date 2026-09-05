#!/usr/bin/env bash
# Host validation for the WebGPU JS/TS carpets. Runs them on Deno (V8 + a built-in copy of gfx-rs
# wgpu-core, the same runtime the on-target gate uses) against Mesa lavapipe (software Vulkan on the
# CPU), mirroring run-webgpu.sh exactly. Deno runs .js and .ts natively - no tsc, no node_modules.
# Gates on each cell's "<MARKER> OK <n>" marker.
#
# Verified call chain (source + strace, not assumed): navigator.gpu -> deno_webgpu -> wgpu-core ->
# wgpu-hal -> ash + libloading -> dlopen libvulkan.so.1 -> lvp_icd.json -> libvulkan_lvp.so (lavapipe)
# -> libgallium + libLLVM (llvmpipe CPU JIT). The engine is wgpu-core, the same one Firefox/Servo use
# and cpu-wgpu-compute #1576 builds on musl.
#
# Env:
#   DENO     path to the deno binary (default: from PATH)
#   VK_ICD   path to the lavapipe ICD json (default: /usr/share/vulkan/icd.d/lvp_icd.json)
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARPETS="$HERE/carpets"
DENO="${DENO:-$(command -v deno 2>/dev/null || echo deno)}"

VK_ICD="${VK_ICD:-/usr/share/vulkan/icd.d/lvp_icd.json}"
export VK_DRIVER_FILES="$VK_ICD"
export VK_ICD_FILENAMES="$VK_ICD"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}"
export DENO_DIR="${DENO_DIR:-/tmp/deno}"
# Single-thread the llvmpipe/lavapipe rasterizer (StarryOS runs one vCPU; the carpets assert numerical
# correctness, not throughput, so thread count does not change the pass counts).
export LP_NUM_THREADS="${LP_NUM_THREADS:-1}"

if ! command -v "$DENO" >/dev/null 2>&1; then
    echo "webgpu: deno not found; host validation needs Deno (built-in WebGPU = wgpu-core)"
    echo "TEST FAILED"
    exit 1
fi

pass=0; fail=0
# run <name> <marker> <file> - a cell passes when its output carries "<marker> OK <n>".
run() {
    name="$1"; marker="$2"; file="$3"
    out="$("$DENO" run --no-check --unstable-webgpu --allow-all "$file" 2>&1)"
    if echo "$out" | grep -qE "^$marker OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|^$marker OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name"
        fail=$((fail + 1))
    fi
}

run webgpu_js WEBGPU_JS_FULL_API "$CARPETS/webgpu_js/webgpu_js_full_api.js"
run webgpu_ts WEBGPU_TS_FULL_API "$CARPETS/webgpu_ts/webgpu_ts_full_api.ts"

total=$((pass + fail))
echo "webgpu: $pass/$total cells OK on $(uname -m) via Deno (wgpu-core) + lavapipe"
if [ "$fail" -eq 0 ] && [ "$pass" -ge 2 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
