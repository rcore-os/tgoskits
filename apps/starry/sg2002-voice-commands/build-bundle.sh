#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 3 ]]; then
    echo "Usage: $0 SHERPA_RISCV_SDK CLANG_TOOLCHAIN_FILE OUTPUT_DIRECTORY" >&2
    exit 2
fi
app_dir="$(cd "$(dirname "$0")" && pwd)"
sdk="$(realpath "$1")"
toolchain="$(realpath "$2")"
output="$(realpath -m "$3")"
workspace="$(git -C "$app_dir" rev-parse --show-toplevel)"
build="$workspace/target/voice-commands-riscv64"
mkdir -p "$build"
exec 9>"$build/build.lock"
flock -n 9 || { echo "Bundle build already running" >&2; exit 1; }
cmake -S "$app_dir" -B "$build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_TOOLCHAIN_FILE="$toolchain" \
    -DSHERPA_ONNX_ROOT="$sdk" -DVOICE_TPU_FRONTEND=OFF
cmake --build "$build"
mkdir -p "$output/bin" "$output/lib" "$output/validation"
install -m 0755 "$build/voice-commands" "$output/bin/"
# This private glibc closure is invoked explicitly; the system musl loader stays
# owned by the rootfs. Dereference SDK symlinks so none escape the bundle.
for file in "$sdk/lib/"*.so*; do
    install -m 0755 "$file" "$output/lib/"
done
for file in "$output/lib/"*.so*; do
    [[ -e "$sdk/lib/${file##*/}" ]] || rm -- "$file"
done
patchelf --set-rpath "\$ORIGIN/../lib" "$output/bin/voice-commands"
python3 "$app_dir/prepare-runtime.py" "$output/bin/voice-commands" "$output/lib/"*.so*
python3 "$app_dir/prepare-model.py" "$workspace/target/voice-model"
mkdir -p "$output/model"
cp -a "$workspace/target/voice-model/." "$output/model/"
install -m 0755 "$app_dir/"{run.sh,listen.sh,validate.sh} "$output/"
install -m 0644 "$app_dir/validation/"*.wav "$output/validation/"
install -m 0644 "$app_dir/validation/provenance.txt" "$output/validation/"
python3 - "$output/validation" <<'PCM'
import pathlib, sys, wave
root = pathlib.Path(sys.argv[1])
with wave.open(str(root / "commands.wav")) as audio:
    assert (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) == (1, 2, 16000)
    pcm = audio.readframes(audio.getnframes())
(root / "repeated.pcm").write_bytes(bytes(10 * 16000 * 2) + pcm + pcm)
PCM
cp -a "$sdk/licenses" "$output/"
install -m 0644 "$app_dir/README.md" "$output/"
printf 'Bundle ready: %s\n' "$output"
