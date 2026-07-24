#!/usr/bin/env bash
# prebuild.sh - provision the host runtime for the WebGPU compute-API carpet and stage the on-target
# launcher.
#
# The WebGPU cells (webgpu_js / webgpu_ts / webgpu_kotlin) run on Node against the dawn native addon
# (the `webgpu` npm package), which loads the Vulkan loader, which loads the Mesa lavapipe ICD
# (software Vulkan on the CPU). Node, the dawn addon, and kotlinc-js are host tools with no StarryOS
# build, so the cells are validated on the host by programs/run_all.sh. This script:
#   1. installs the `webgpu` npm package (dawn addon) + tsc + @webgpu/types into programs/carpets/
#      webgpu_js/node_modules (host), and
#   2. stages the on-target overlay (run-webgpu.sh), which reports the host-only nature honestly.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR,
# STARRY_APP_DIR. The rootfs/overlay staging is only used to place the launcher script.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
PROG="$app_dir/programs"
JS="$PROG/carpets/webgpu_js"
TS="$PROG/carpets/webgpu_ts"

# --- host: install the webgpu (dawn) npm package + tsc so run_all.sh can run the JS/TS cells -------
install_host_npm() {
    if ! command -v npm >/dev/null 2>&1; then
        echo "prebuild: npm not on PATH; install Node + npm to build the host webgpu runtime" >&2
        return 0
    fi
    if [[ -d "$JS/node_modules/webgpu" ]]; then
        echo "prebuild: host webgpu (dawn) npm package already present in $JS/node_modules"
        return 0
    fi
    echo "prebuild: npm install (webgpu dawn addon + typescript + @webgpu/types) in $JS ..."
    ( cd "$JS" && npm install --no-audit --no-fund )
}

# --- host: sanity-check the lavapipe ICD the dawn addon will load ---------------------------------
check_lavapipe() {
    local icd="${VK_ICD:-/usr/share/vulkan/icd.d/lvp_icd.json}"
    if [[ -f "$icd" ]]; then
        echo "prebuild: lavapipe ICD found at $icd"
    else
        echo "prebuild: WARNING lavapipe ICD not found at $icd; install mesa-vulkan-drivers (lavapipe)"
        echo "prebuild: set VK_ICD=<path to lvp_icd.json> if the ICD lives elsewhere"
    fi
}

# --- on-target: stage the launcher that reports the host-only nature ------------------------------
stage_overlay() {
    [[ -n "$overlay_dir" ]] || { echo "prebuild: no STARRY_OVERLAY_DIR; skipping on-target overlay"; return 0; }
    install -Dm0755 "$PROG/run-webgpu.sh" "$overlay_dir/usr/bin/run-webgpu.sh"
    echo "prebuild: staged run-webgpu.sh into overlay"
}

install_host_npm
check_lavapipe
stage_overlay
echo "prebuild: WebGPU carpet host runtime ready; run 'bash programs/run_all.sh' to validate"
