#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:?prebuild: STARRY_APP_DIR is required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH is required}"
workspace="${STARRY_WORKSPACE:?prebuild: STARRY_WORKSPACE is required}"

case "$arch" in
    aarch64) triple="aarch64-linux-musl" ;;
    *) echo "prebuild: starryos-task2 currently supports only aarch64" >&2; exit 1 ;;
esac

cc="${CROSS_CC:-/home/huhu/.local/toolchains/${triple}-cross/bin/${triple}-gcc}"
if [[ ! -x "$cc" ]]; then
    if command -v "${triple}-gcc" >/dev/null 2>&1; then
        cc="$(command -v "${triple}-gcc")"
    else
        echo "prebuild: no musl compiler for $triple" >&2
        exit 1
    fi
fi
cxx="${CROSS_CXX:-/home/huhu/.local/toolchains/${triple}-cross/bin/${triple}-g++}"
ar="${CROSS_AR:-/home/huhu/.local/toolchains/${triple}-cross/bin/${triple}-ar}"
for tool in "$cxx" "$ar"; do
    if [[ ! -x "$tool" ]]; then
        echo "prebuild: missing musl cross tool: $tool" >&2
        exit 1
    fi
done

ncnn_prefix="${NCNN_PREFIX:-$workspace/tmp/task3-yolo/ncnn-aarch64/install}"
yolo_assets="${TASK3_YOLO_ASSETS:-$workspace/tmp/task3-yolo/ncnn-model}"
ab_manifest="$workspace/scripts/task3/task3-ab-manifest.tsv"
ab_assets="$yolo_assets/task3-ab"
if [[ ! -f "$ncnn_prefix/include/ncnn/net.h" || ! -f "$ncnn_prefix/lib/libncnn.a" ]]; then
    echo "prebuild: incomplete ncnn installation: $ncnn_prefix" >&2
    exit 1
fi

verify_asset() {
    local name="$1"
    local expected_sha256="$2"
    local path="$yolo_assets/$name"
    if [[ ! -f "$path" ]]; then
        echo "prebuild: missing YOLO asset: $path" >&2
        exit 1
    fi
    local actual_sha256
    actual_sha256="$(sha256sum "$path" | awk '{print $1}')"
    if [[ "$actual_sha256" != "$expected_sha256" ]]; then
        echo "prebuild: YOLO asset hash mismatch for $name" >&2
        echo "prebuild: expected $expected_sha256, got $actual_sha256" >&2
        exit 1
    fi
}

verify_asset yolo11n.ncnn.param d2c0adf8939dc9ce02964ce8ada104447768ffd8e3bffad8fa11e2e61e709c1f
verify_asset yolo11n.ncnn.bin 0ae562447923999779b12b4f91f96b9ef263add8c9902d10e22e6dd6a2932c12
verify_asset input.ppm 608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f
"$workspace/scripts/task3/prepare-yolo-ncnn-ab-inputs.sh" >/dev/null

build_dir="$workspace/target/starryos-task2-rust"
rm -rf "$build_dir"
mkdir -p "$build_dir"
linker_dir="$build_dir/linker"
mkdir -p "$linker_dir"
ln -sf "$cc" "$linker_dir/aarch64-unknown-linux-musl-ld"

CXX_aarch64_unknown_linux_musl="$cxx" \
AR_aarch64_unknown_linux_musl="$ar" \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$cc" \
NCNN_PREFIX="$ncnn_prefix" \
RUSTFLAGS="-C target-feature=+crt-static" \
cargo build --release --target aarch64-unknown-linux-musl \
    --manifest-path "$app_dir/rust/Cargo.toml" --target-dir "$build_dir"
out="$build_dir/aarch64-unknown-linux-musl/release/starryos-task2-endpoint"
test -x "$out"

install -Dm0755 "$out" "$overlay_dir/usr/bin/starry-udp-probe"
install -Dm0755 "$app_dir/udp-probe.sh" "$overlay_dir/usr/bin/starry-udp-probe.sh"
install -Dm0755 "$out" "$overlay_dir/usr/bin/starry-t2n1-endpoint"
install -Dm0755 "$app_dir/t2n1-run.sh" "$overlay_dir/usr/bin/t2n1-run.sh"
install -Dm0644 "$yolo_assets/yolo11n.ncnn.param" \
    "$overlay_dir/usr/share/task3-yolo/yolo11n.ncnn.param"
install -Dm0644 "$yolo_assets/yolo11n.ncnn.bin" \
    "$overlay_dir/usr/share/task3-yolo/yolo11n.ncnn.bin"
install -Dm0644 "$yolo_assets/input.ppm" \
    "$overlay_dir/usr/share/task3-yolo/input.ppm"
install -Dm0644 "$ab_manifest" \
    "$overlay_dir/usr/share/task3-yolo/task3-ab/manifest.tsv"
while IFS=$'\t' read -r image_id filename expected_sha256 truth_target expected_behavior; do
    [[ -z "$image_id" || "$image_id" == \#* ]] && continue
    actual_sha256="$(sha256sum "$ab_assets/$filename" | awk '{print $1}')"
    if [[ "$actual_sha256" != "$expected_sha256" ]]; then
        echo "prebuild: Task-3 A/B image hash mismatch for $image_id" >&2
        exit 1
    fi
    install -Dm0644 "$ab_assets/$filename" \
        "$overlay_dir/usr/share/task3-yolo/task3-ab/$filename"
done < "$ab_manifest"
echo "prebuild: starryos-task2 ncnn/YOLO endpoint built for $arch"
