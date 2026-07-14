#!/usr/bin/env bash
set -euo pipefail

export STARRY_APP_DIR=/workspace/apps/starry/doom
export STARRY_ARCH=x86_64
export STARRY_ROOTFS=/tmp/.tgos-images/rootfs-x86_64-alpine.img/rootfs-x86_64-alpine.img
export STARRY_STAGING_ROOT=/workspace/tmp/doom-staging
export STARRY_OVERLAY_DIR=/workspace/tmp/doom-overlay
export STARRY_WORKSPACE=/workspace

rm -rf "$STARRY_STAGING_ROOT" "$STARRY_OVERLAY_DIR"
mkdir -p "$STARRY_STAGING_ROOT" "$STARRY_OVERLAY_DIR"

cd /workspace
bash /workspace/apps/starry/doom/prebuild.sh 2>&1
echo "EXIT_CODE=$?"
