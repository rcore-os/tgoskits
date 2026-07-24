#!/usr/bin/env bash
# Compile + run the webgpu x Kotlin carpet with the standalone Kotlin/JS IR backend (no Gradle) and
# run it on Node against the dawn-based `webgpu` npm package on Mesa lavapipe.
#
# Kotlin 2.x JS is IR-only and normally Gradle-driven, but the IR backend works standalone in two
# steps: (1) compile .kt -> .klib, (2) link the .klib -> CommonJS .js. The `-Xir-produce-js` step
# WIPES the output dir, so the async trampoline helper and the node_modules symlink are placed AFTER
# linking.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KOTLINC="${KOTLINC:-$(command -v kotlinc-js 2>/dev/null || echo kotlinc-js)}"
KHOME="$(dirname "$(dirname "$KOTLINC")")"
STDLIB="$KHOME/lib/kotlin-stdlib-js.klib"
NODE_BIN="${NODE_BIN:-$(dirname "$(command -v node 2>/dev/null || echo node)")}"
export PATH="$NODE_BIN:$PATH"
export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk-amd64}"

BUILD="$HERE/build"
rm -rf "$BUILD"
mkdir -p "$BUILD/klib" "$BUILD/out"

echo ">> step 1: kotlin -> klib"
"$KOTLINC" \
  -Xir-produce-klib-file \
  -libraries "$STDLIB" \
  -ir-output-dir "$BUILD/klib" \
  -ir-output-name webgpu_kotlin \
  -Xir-module-name=webgpu_kotlin \
  "$HERE/webgpu_kotlin.kt"

echo ">> step 2: klib -> js (commonjs)"
"$KOTLINC" \
  -Xir-produce-js \
  -Xinclude="$BUILD/klib/webgpu_kotlin.klib" \
  -libraries "$STDLIB" \
  -ir-output-dir "$BUILD/out" \
  -ir-output-name webgpu_kotlin \
  -module-kind commonjs \
  -main call

# the js-producing step wipes build/out, so wire up runtime deps now
cp "$HERE/webgpu_kotlin_await.js" "$BUILD/out/webgpu_kotlin_await.js"
ln -sfn "$HERE/node_modules" "$BUILD/out/node_modules"

echo ">> run on lavapipe"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/webgpu-kotlin-xdg}"
mkdir -p "$XDG_RUNTIME_DIR"
export VK_DRIVER_FILES="${VK_DRIVER_FILES:-/usr/share/vulkan/icd.d/lvp_icd.json}"
# Single-thread llvmpipe: the dawn addon's native callback threads otherwise race llvmpipe (glibc
# pthread_mutex assertion / SIGSEGV). This matches StarryOS single-vCPU execution.
export LP_NUM_THREADS="${LP_NUM_THREADS:-1}"
exec node "$BUILD/out/webgpu_kotlin.js"
