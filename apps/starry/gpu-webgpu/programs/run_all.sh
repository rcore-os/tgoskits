#!/usr/bin/env bash
# Host runner for the WebGPU compute-API carpet cells. Runs the JavaScript and TypeScript cells on
# Node against the dawn-based `webgpu` npm package on Mesa lavapipe (software Vulkan on the CPU), and
# gates on each cell's "<NAME> OK <n>" marker.
#
# WebGPU here is exercised through the dawn native addon (the `webgpu` npm package), which loads the
# Vulkan loader, which loads the lavapipe ICD. Node, dawn, and kotlinc-js are host tools with no
# StarryOS build, so these cells validate on the host; the on-target rootfs run (run-webgpu.sh)
# reports this honestly and does not fake a device.
#
# Env:
#   VK_ICD          path to the lavapipe ICD json (default: system /usr/share/vulkan/icd.d/lvp_icd.json)
#   NODE_BIN        directory holding the node binary (default: from PATH)
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARPETS="$HERE/carpets"
JS="$CARPETS/webgpu_js"
TS="$CARPETS/webgpu_ts"

NODE_BIN="${NODE_BIN:-$(dirname "$(command -v node 2>/dev/null || echo node)")}"
export PATH="$NODE_BIN:$PATH"

VK_ICD="${VK_ICD:-/usr/share/vulkan/icd.d/lvp_icd.json}"
export VK_DRIVER_FILES="$VK_ICD"
export VK_ICD_FILENAMES="$VK_ICD"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}"
# Single-thread the llvmpipe/lavapipe rasterizer. StarryOS runs one vCPU, and on the host dawn's
# native callback threads otherwise race the multi-threaded rasterizer. The carpets assert numerical
# correctness, not throughput, so thread count does not change the pass counts.
export LP_NUM_THREADS="${LP_NUM_THREADS:-1}"

if ! command -v node >/dev/null 2>&1; then
    echo "webgpu: node not found on PATH; JS/TS cells need Node + the webgpu (dawn) npm package"
    echo "TEST FAILED"
    exit 1
fi
if [ ! -d "$JS/node_modules/webgpu" ]; then
    echo "webgpu: $JS/node_modules/webgpu missing; run 'npm install' in $JS first"
    echo "TEST FAILED"
    exit 1
fi

pass=0; fail=0
# run <name> <marker> <cmd...> - a cell passes when its output carries "<marker> OK <n>".
run() {
    name="$1"; marker="$2"; shift 2
    out="$("$@" 2>&1)"
    if echo "$out" | grep -qE "^$marker OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|^$marker OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name"
        fail=$((fail + 1))
    fi
}

# --- JS cell ---------------------------------------------------------------------------------------
run webgpu_js WEBGPU_JS_FULL_API node "$JS/webgpu_js_full_api.js"

# --- TS cell: type-check + compile with the pinned tsc, then run the emitted JS -------------------
TSC="$JS/node_modules/.bin/tsc"
if [ -x "$TSC" ]; then
    ( cd "$TS" && [ -e node_modules ] || ln -sfn "$JS/node_modules" node_modules )
    if ( cd "$TS" && "$TSC" -p tsconfig.json ); then
        run webgpu_ts WEBGPU_TS_FULL_API node "$TS/webgpu_ts_full_api.js"
    else
        echo "CARPET FAILED: webgpu_ts (tsc type-check failed)"
        fail=$((fail + 1))
    fi
else
    echo "webgpu_ts: tsc not found under $JS/node_modules/.bin; skipping TS cell"
fi

# --- Kotlin cell: host wall -----------------------------------------------------------------------
# The Kotlin/JS cell source (webgpu_kotlin/webgpu_kotlin.kt, 78 pinned assertions) is complete and
# compiles with the Kotlin/JS IR backend, but it requires kotlinc-js, and on hosts that have it the
# Kotlin/JS coroutine continuation crashes the dawn native addon on re-entry after a compute pipeline
# exists (isolated in webgpu_kotlin/wall-evidence/FINDING.md). It is therefore not gated here.
if command -v kotlinc-js >/dev/null 2>&1; then
    echo "webgpu_kotlin: kotlinc-js present; cell documented as a host wall (see wall-evidence/FINDING.md), not gated"
else
    echo "webgpu_kotlin: kotlinc-js absent on this host; cell source complete but not built (wall)"
fi

total=$((pass + fail))
echo "webgpu: $pass/$total cells OK on $(uname -m)"
if [ "$fail" -eq 0 ] && [ "$pass" -ge 2 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
