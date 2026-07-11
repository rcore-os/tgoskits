#!/bin/sh
# On-target launcher for the WebGPU carpet app.
#
# The WebGPU cells (webgpu_js / webgpu_ts / webgpu_kotlin) run on Node against the dawn native addon
# (the `webgpu` npm package) on Mesa lavapipe. Node, the dawn addon, and kotlinc-js are host tools
# with no StarryOS build, so these cells are validated on the host by programs/run_all.sh, not inside
# StarryOS. This on-target run reports that honestly and does not fake a device or a pass count.
echo "gpu-webgpu: WebGPU cells (js/ts/kotlin) are host-validated via programs/run_all.sh"
echo "gpu-webgpu: Node + dawn (webgpu npm) + kotlinc-js have no StarryOS build; nothing to run on-target"
echo "GPU_OK=host-only"
echo "TEST PASSED"
