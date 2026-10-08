#!/usr/bin/env bash
# Ubuntu 24.04 host; output is private glibc, never installed over system musl.
set -euo pipefail
for tool in clang clang++ ld.lld cmake ninja git curl unzip dpkg-deb apt patchelf sha256sum python3 readelf flock timeout; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done
APP=$(cd "$(dirname "$0")" && pwd)
ROOT=$(realpath "${1:-$(git rev-parse --show-toplevel)}")
DEPS="$ROOT/target/voice-deps"; WORK="$ROOT/target/voice-riscv-runtime"
SRC="$DEPS/sherpa-source"; SYS="$DEPS/sysroot"
SDK="$WORK/sdk"
mkdir -p "$DEPS/sysroot-debs" "$WORK/build" "$SDK/lib" "$SDK/include/sherpa-onnx/c-api"
exec 9>"$WORK/build-runtime.lock"; flock -n 9 || { echo "Runtime build already running" >&2; exit 1; }
verify() { printf '%s  %s\n' "$2" "$1" | sha256sum -c -; }
fetch() {
  local file=$1 sha=$2 url=$3 fallback=${4:-} cached
  if [[ ! -f "$file" && -d "$WORK/build/_deps" ]]; then
    cached=$(find "$WORK/build/_deps" -path "*/src/${file##*/}" -type f -print -quit)
    [[ -z "$cached" ]] || { verify "$cached" "$sha"; cp "$cached" "$file"; }
  fi
  if [[ ! -f "$file" ]]; then
    if ! curl -fL --connect-timeout 15 --max-time 120 --max-filesize 50000000 "$url" -o "$file.part"; then
      [[ -n "$fallback" ]] || return 1
      curl -fL --connect-timeout 15 --max-time 120 --max-filesize 50000000 "$fallback" -o "$file.part"
    fi
    verify "$file.part" "$sha"; mv "$file.part" "$file"
  fi
  verify "$file" "$sha"
}
# 1. Pinned Ubuntu cross packages: Linux/glibc headers, runtime, GCC C++ support.
while read -r name version sha; do
  deb="$DEPS/sysroot-debs/${name}_${version}_all.deb"
  [[ -f "$deb" ]] || (cd "$DEPS/sysroot-debs"; timeout 120 apt download "$name=$version")
  verify "$deb" "$sha"; dpkg-deb -x "$deb" "$SYS"
done <<'PACKAGES'
linux-libc-dev-riscv64-cross 6.8.0-25.25cross1 d21675d3b0b8865733f03eb50408d776d8d8da4ec71409ac7238ffeaf08b56bc
libc6-riscv64-cross 2.39-0ubuntu8cross1 d0eabe13dbb68e914aa0485da0d4b28843bd80b5066d801cf34d4c00e5b61df2
libc6-dev-riscv64-cross 2.39-0ubuntu8cross1 aa4e752b0cbd32902ec0835cc4f9dc464030c329eda1fb0349e5774acda77f20
libgcc-s1-riscv64-cross 14.2.0-4ubuntu2~24.04.1cross1 288fd6ebaf9fe898bde7f624767b870ea705af3e95f9e6b7548162d3502dd7f4
libstdc++6-riscv64-cross 14.2.0-4ubuntu2~24.04.1cross1 83720d74b0a05a25e2b831e41721c38316855bce9f7092564e8656aad48d3e67
libgcc-13-dev-riscv64-cross 13.3.0-6ubuntu2~24.04.1cross1 fb0fe93ed67202f6c39704203399dd3a4e90fa72254eb0f2328dba1c13277a19
libstdc++-13-dev-riscv64-cross 13.3.0-6ubuntu2~24.04.1cross1 a751da657d2b24b3355563cb67142cb4922d57817bc303189a7325cf69a49e99
PACKAGES
# 2. Official sherpa v1.12.14 source, exact commit; retain an existing checkout.
COMMIT=26aa2fa93210376a89de3a65a1a4dd320c37f5e9
if [[ ! -d "$SRC/.git" ]]; then
  timeout 180 git clone --depth 1 --filter=blob:none --sparse --branch v1.12.14 \
    https://github.com/k2-fsa/sherpa-onnx.git "$SRC"
  git -C "$SRC" sparse-checkout set cmake sherpa-onnx/c-api sherpa-onnx/csrc
fi
[[ $(git -C "$SRC" rev-parse HEAD) == "$COMMIT" ]]
git -C "$SRC" diff --quiet HEAD --
# 3. Debian ORT 1.21: RV64GC, validated with official fp32 2025 KWS models.
ORT="$DEPS/ort-debian-1.21"; mkdir -p "$ORT"
while read -r path sha; do
  deb="$ORT/${path##*/}"; fetch "$deb" "$sha" "https://deb.debian.org/debian/$path"
  dpkg-deb -x "$deb" "$ORT/root"
done <<'ORT_PACKAGES'
pool/main/o/onnxruntime/libonnxruntime1.21_1.21.0+dfsg-1_riscv64.deb 394ce4ae6ce15c5c17b68cbde8fee530215ec4a592ef833dcff9e21a23c40307
pool/main/o/onnxruntime/libonnxruntime-dev_1.21.0+dfsg-1_riscv64.deb 880ec54a7b149ab27b8620db49cab317b14fa2770865afc3de710a95db1cdce7
pool/main/o/onnx/libonnx1t64_1.17.0-3+b1_riscv64.deb 7c3b53d658cce756c3b0ec00d6aab8f5c7be8c0b9b6e18f17935e6091d101863
pool/main/p/protobuf/libprotobuf32t64_3.21.12-11+deb13u1_riscv64.deb 57523cc3e849a2d6a0fca89de8265aad7ddc6a6e433dbd5403aa8b7f89cf9bb5
pool/main/r/re2/libre2-11_20240702-3+b1_riscv64.deb 8a62db56b12a9567760114f683d8ffedf2bdd7d402e124b7b19cf54b838dcee6
pool/main/a/abseil/libabsl20240722_20240722.0-4_riscv64.deb 4bec4936487f8c9cb48f32f0a4f9fccdeefc26dcacd4668abe6a6377ca147eaf
pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_riscv64.deb 38c52eef58e5fc9b13d93f8a7d0a0549c32b4f00402a778d39a135254f554314
ORT_PACKAGES
ORT_INCLUDE="$ORT/root/usr/include/onnxruntime"
ORT_LIB="$ORT/root/usr/lib/riscv64-linux-gnu"
# 4. Reuse verified CMake archives; obtain missing files from upstream mirrors.
for module in kaldi-native-fbank kaldi-decoder simple-sentencepiece cppjieba kaldifst openfst eigen; do
  spec="$SRC/cmake/$module.cmake"
  primary=$(sed -n 's/.*set([^ ]*_URL  *"\([^"]*\)").*/\1/p' "$spec" | head -1)
  mirror=$(sed -n 's/.*set([^ ]*_URL2  *"\([^"]*\)").*/\1/p' "$spec" | head -1)
  sha=$(sed -n 's/.*SHA256=\([0-9a-f]*\).*/\1/p' "$spec" | head -1)
  fetch "$WORK/build/${mirror##*/}" "$sha" "${mirror/hf-mirror.com/huggingface.co}" "$primary"
done
KISS=febd4caeed32e33ad8b2e0bb5ea77542c40f18ec
cached="$WORK/build/_deps/kissfft-subbuild/kissfft-populate-prefix/src/$KISS.zip"
[[ ! -f "$cached" ]] || cp "$cached" "$WORK/build/kissfft-$KISS.zip"
fetch "$WORK/build/kissfft-$KISS.zip" 497103e664168ebe39580b757adbe616f6cf85a16572af581ca7bc42d0ab13fd \
  "https://github.com/mborgerding/kissfft/archive/$KISS.zip"
# 5. Clang GNU cross ABI; -idirafter preserves libstdc++ include_next ordering.
TC="$WORK/toolchain.cmake"
cat >"$TC" <<TOOLCHAIN
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR riscv64)
set(CMAKE_SYSROOT "$SYS")
set(CMAKE_C_COMPILER clang)
set(CMAKE_CXX_COMPILER clang++)
set(CMAKE_C_COMPILER_TARGET riscv64-linux-gnu)
set(CMAKE_CXX_COMPILER_TARGET riscv64-linux-gnu)
set(CMAKE_C_COMPILER_EXTERNAL_TOOLCHAIN "$SYS/usr")
set(CMAKE_CXX_COMPILER_EXTERNAL_TOOLCHAIN "$SYS/usr")
set(CMAKE_C_FLAGS_INIT "-march=rv64gc -mabi=lp64d -idirafter $SYS/usr/riscv64-linux-gnu/include")
set(CMAKE_CXX_FLAGS_INIT "-march=rv64gc -mabi=lp64d -idirafter $SYS/usr/riscv64-linux-gnu/include")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_FIND_ROOT_PATH "$SYS/usr/riscv64-linux-gnu")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
TOOLCHAIN
export SHERPA_ONNXRUNTIME_INCLUDE_DIR="$ORT_INCLUDE" SHERPA_ONNXRUNTIME_LIB_DIR="$ORT_LIB"
flags=()
for option in TTS SPEAKER_DIARIZATION PYTHON JNI TESTS BINARY PORTAUDIO WEBSOCKET; do
  flags+=("-DSHERPA_ONNX_ENABLE_$option=OFF")
done
# Keep the source checkout pristine; only this dedicated build gets the hook.
# Reversing the exact patch preserves unrelated edits rather than resetting Git.
git -C "$SRC" apply --unidiff-zero "$APP/sherpa-frontend-hook.patch"
restore_source() {
  local status=$?
  git -C "$SRC" apply --unidiff-zero -R "$APP/sherpa-frontend-hook.patch" || status=1
  trap - EXIT
  exit "$status"
}
trap restore_source EXIT
timeout 180 cmake -S "$SRC" -B "$WORK/build" -G Ninja -DCMAKE_TOOLCHAIN_FILE="$TC" \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON -DSHERPA_ONNX_ENABLE_C_API=ON \
  -DSHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF "${flags[@]}" >"$WORK/configure.log" 2>&1
timeout 600 cmake --build "$WORK/build" --target sherpa-onnx-c-api --parallel "${JOBS:-6}" >"$WORK/build.log" 2>&1
cp "$WORK/build/lib/libsherpa-onnx-c-api.so" "$SDK/lib/"
cp "$SRC/sherpa-onnx/c-api/c-api.h" "$SDK/include/sherpa-onnx/c-api/"
mkdir -p "$SDK/include/onnxruntime"
cp "$ORT_INCLUDE/"*.h "$SDK/include/onnxruntime/"
patchelf --set-rpath "\$ORIGIN" "$SDK/lib/libsherpa-onnx-c-api.so"
(cd "$SDK"; sha256sum lib/libsherpa-onnx-c-api.so > frontend-hook.sha256)
# 6. Copy only the recursive DT_NEEDED closure, plus the matching C header.
python3 - "$SDK/lib" "$ORT_LIB" "$SYS/usr/riscv64-linux-gnu/lib" <<'CLOSURE'
import pathlib, re, shutil, subprocess, sys
dest = pathlib.Path(sys.argv[1]); roots = [pathlib.Path(p) for p in sys.argv[2:]]
pending = [dest / "libsherpa-onnx-c-api.so"]; done = {pending[0].name}
while pending:
    elf = pending.pop()
    needed = re.findall(r"\(NEEDED\).*\[(.*?)\]", subprocess.check_output(["readelf", "-d", str(elf)], text=True))
    for name in needed:
        if name in done: continue
        done.add(name)
        source = next((root / name for root in roots if (root / name).exists()), None)
        if source is None: raise RuntimeError(f"Missing shared library: {name}")
        target = dest / name; shutil.copy2(source, target); pending.append(target)
for stale in dest.glob("*.so*"):
    if stale.name not in done:
        stale.unlink()
CLOSURE
chmod +x "$SDK/lib/ld-linux-riscv64-lp64d.so.1"
mkdir -p "$SDK/licenses"
cp "$SRC/LICENSE" "$SDK/licenses/sherpa-onnx-LICENSE"
for root in "$SYS/usr/share/doc" "$ORT/root/usr/share/doc"; do
  for notice in "$root"/*/copyright; do
    [[ -f "$notice" ]] || continue
    package=$(basename "$(dirname "$notice")")
    cp -L "$notice" "$SDK/licenses/$package-copyright"
  done
done
echo "SDK: $SDK"
echo 'Run: BUNDLE/lib/ld-linux-riscv64-lp64d.so.1 --library-path BUNDLE/lib BUNDLE/bin/voice-commands ...'
