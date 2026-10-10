#!/usr/bin/env bash
# The CPU application and TPU worker intentionally use separate libc runtimes.
set -euo pipefail
if [[ $# != 4 ]]; then
    echo "Usage: $0 SHERPA_RISCV_SDK CLANG_TOOLCHAIN_FILE CVI_SDK OUTPUT_DIRECTORY" >&2
    echo "Set TPU_PYTHON to the TPU-MLIR 1.30.2 Python and CVI_CC to the musl cross compiler." >&2
    exit 2
fi
app=$(cd "$(dirname "$0")" && pwd)
workspace=$(git -C "$app" rev-parse --show-toplevel)
sdk=$(realpath "$1")
toolchain=$(realpath "$2")
cvi=$(realpath "$3")
output=$(realpath -m "$4")
python=$(command -v "${TPU_PYTHON:?Set TPU_PYTHON}")
cc=$(command -v "${CVI_CC:?Set CVI_CC}")
strip=${cc%gcc}strip
[[ ! -e "$output" ]] || { echo "Output already exists: $output" >&2; exit 1; }
[[ $("$cc" -dumpmachine) == riscv64*-linux-musl ]]
[[ -x "$strip" ]]
(cd "$sdk"; sha256sum -c frontend-hook.sha256)
work="$workspace/target/voice-tpu-build"
mkdir -p "$work" "$(dirname "$output")"
exec 8>"$work/build.lock"
flock -n 8 || { echo "TPU bundle build already running" >&2; exit 1; }
stage=$(mktemp -d "${output}.partial.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT

bash "$app/build-bundle.sh" "$sdk" "$toolchain" "$stage"
cmake -S "$app" -B "$work/bridge" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_TOOLCHAIN_FILE="$toolchain" -DSHERPA_ONNX_ROOT="$sdk" -DVOICE_TPU_FRONTEND=ON
cmake --build "$work/bridge" --target voice_frontend
install -m 0755 "$work/bridge/libvoice_frontend.so" "$stage/lib/"
python3 "$app/prepare-runtime.py" "$stage/lib/libvoice_frontend.so"

mkdir -p "$stage/tpu/lib"
for library in libcviruntime.so libcvikernel.so; do
    install -m 0755 "$cvi/lib/$library" "$stage/tpu/lib/$library"
done
for library in libstdc++.so.6 libgcc_s.so.1; do
    install -m 0755 "$("$cc" -print-file-name="$library")" "$stage/tpu/lib/$library"
done
"$cc" -O2 -march=rv64gc -mabi=lp64d -std=c11 -Wall -Wextra -Werror \
    -I"$cvi/include" "$app/frontend-worker.c" -L"$cvi/lib" -lcviruntime \
    -Wl,-rpath-link,"$cvi/lib" -Wl,-rpath,"\$ORIGIN/lib" -o "$stage/tpu/frontend-worker"
"$strip" --strip-unneeded "$stage/tpu/frontend-worker" "$stage/tpu/lib/"*.so*
python3 "$app/prepare-runtime.py" "$stage/tpu/frontend-worker" "$stage/tpu/lib/"*.so*

"$python" "$app/prepare-tpu-model.py" "$stage/model/encoder.onnx" "$work/model"
install -m 0644 "$work/model/"{encoder.onnx,frontend.cvimodel,tpu-model.json} "$stage/model/"
install -m 0755 "$app/run-tpu.sh" "$stage/run.sh"
# Record the actual SDK/compiler inputs without vendoring upstream SDK binaries.
{
    printf 'CVI_CC: '; "$cc" --version | head -n 1
    printf 'TPU-MLIR: '; "$python" -c 'import importlib.metadata; print(importlib.metadata.version("tpu-mlir"))'
    (cd "$cvi"; sha256sum include/cviruntime.h include/cviruntime_context.h \
        include/cvitpu_debug.h lib/libcviruntime.so lib/libcvikernel.so)
} > "$stage/tpu/build-inputs.txt"
mv -T -- "$stage" "$output"
trap - EXIT
printf 'TPU bundle ready: %s\n' "$output"
