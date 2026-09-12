#!/bin/sh
# On-target WebGPU JS/TS carpet runner for StarryOS.
#
# WebGPU standalone runtime = Deno (V8 + a built-in copy of gfx-rs wgpu-core, the Rust WebGPU engine
# Firefox and Servo also use, and the exact engine cpu-wgpu-render #1820 builds on musl). Deno runs
# .js and .ts natively; its global navigator.gpu drives wgpu-core -> the Vulkan backend -> Mesa
# lavapipe (software Vulkan on the CPU), so the carpets run entirely on the CPU, no GPU. Alpine ships a
# native-musl Deno only for x86_64, so this is the x86_64 on-target gate; the aarch64/riscv64/
# loongarch64 walls (and the browser path #391) are documented in README.md.
#
# The gate is manifest-honest: prebuild.sh writes expected_cells listing exactly the cells whose
# runtime it provisioned on this arch. This runner passes only when every listed cell prints its
# "<MARKER> OK <n>" marker (fail==0 && total==EXPECTED==pass, EXPECTED>=2). It never emits a 0-carpet
# TEST PASSED.
set -u
APP=/opt/cpu-webgpu-render
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
    echo "cpu-webgpu-render: no WebGPU runtime provisioned on $(uname -m) (manifest empty)"
    echo "cpu-webgpu-render: standalone Deno WebGPU is x86_64-only on musl; full JS/TS WebGPU on this"
    echo "cpu-webgpu-render: arch is the browser path (#391). Refusing to emit a 0-carpet pass."
    echo "TEST FAILED"
    exit 1
fi

DENO="$(command -v deno 2>/dev/null || echo /usr/bin/deno)"
if [ ! -x "$DENO" ]; then
    echo "cpu-webgpu-render: manifest lists cells but deno is missing - provisioning broken"
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

run webgpu_js WEBGPU_RENDER_JS_FULL_API "$CARPETS/webgpu_js/webgpu_render_js_full_api.js"
run webgpu_ts WEBGPU_RENDER_TS_FULL_API "$CARPETS/webgpu_ts/webgpu_render_ts_full_api.ts"
# Render-scene cells: 4 scenarios x {js, ts}, mirroring the wgpu Rust render-scene cells.
run scene_2dui_js SCENE_2DUI_JS "$CARPETS/scene_2dui_js/scene_2dui_js.js"
run scene_2dui_ts SCENE_2DUI_TS "$CARPETS/scene_2dui_ts/scene_2dui_ts.ts"
run scene_3dmodel_js SCENE_3DMODEL_JS "$CARPETS/scene_3dmodel_js/scene_3dmodel_js.js"
run scene_3dmodel_ts SCENE_3DMODEL_TS "$CARPETS/scene_3dmodel_ts/scene_3dmodel_ts.ts"
run scene_anim_js SCENE_ANIM_JS "$CARPETS/scene_anim_js/scene_anim_js.js"
run scene_anim_ts SCENE_ANIM_TS "$CARPETS/scene_anim_ts/scene_anim_ts.ts"
run scene_codec_js SCENE_CODEC_JS "$CARPETS/scene_codec_js/scene_codec_js.js"
run scene_codec_ts SCENE_CODEC_TS "$CARPETS/scene_codec_ts/scene_codec_ts.ts"

total=$((pass + fail))
echo "cpu-webgpu-render: $pass/$total WebGPU cells OK on $(uname -m) via Deno (wgpu-core) + lavapipe"
if [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ] && [ "$EXPECTED" -ge 2 ]; then
    echo "TEST PASSED"
else
    echo "TEST FAILED"
fi
