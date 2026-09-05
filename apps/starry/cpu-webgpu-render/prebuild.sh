#!/usr/bin/env bash
# prebuild.sh - provision the on-target WebGPU JS/TS runtime and stage the carpets into the per-arch
# Alpine rootfs.
#
# WebGPU standalone runtime = Deno (V8 + a built-in copy of gfx-rs wgpu-core, the Rust WebGPU engine
# Firefox and Servo also use, and the exact engine cpu-wgpu-render #1820 builds on musl). Deno runs
# .js and .ts natively; its global navigator.gpu drives wgpu-core -> the Vulkan backend -> Mesa
# lavapipe (software Vulkan on the CPU), so the JS/TS WebGPU render carpets (offscreen render -> RGBA8 readback -> per-pixel
# closed-form assertions) run entirely on the CPU with no GPU. This script extracts the base Alpine rootfs, `apk add`s mesa-vulkan-swrast (lavapipe) + the
# Vulkan loader + (x86_64 only) deno via qemu-user-static, then stages Deno + the lavapipe closure +
# the webgpu_js / webgpu_ts carpets + a capability manifest into the overlay.
#
# Arch reality (verified against the Alpine package DB + rusty_v8 release assets, not assumed):
#   - x86_64: Alpine edge/community ships a native-musl `deno` (2.7.4-r2) -> on-target JS/TS gate.
#   - aarch64: Alpine edge/community ALSO ships a native-musl `deno` (rusty_v8 v150.2.0+ carries an
#     aarch64-unknown-linux-musl static V8 lib) -> on-target JS/TS gate, same as x86_64.
#   - riscv64 / loongarch64: no Alpine deno yet (rusty_v8 ships only riscv64-gnu, no loong at all).
#     These arches are brought up in a follow-up (riscv64 via the community gnu prebuilt / from-source,
#     loong via the V8-loong64 backend port into rusty_v8) and are not advertised here yet.
# Both the C-side four-arch WebGPU *compute* (cpu-wgpu-render #1820, same wgpu-core) and the browser
# WebGPU (campaign #391: Chromium=Dawn, Firefox/Servo/Deno=wgpu-core) sit alongside this app.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
PROG="$app_dir/programs"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static" ;;
    riscv64)     qemu_runner="qemu-riscv64-static" ;;
    x86_64)      qemu_runner="qemu-x86_64-static" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    if [[ ${#missing[@]} -gt 0 ]]; then
        command -v apt-get >/dev/null 2>&1 && apt-get update && apt-get install -y --no-install-recommends "${missing[@]}" \
            || { echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1; }
    fi
}

# The harness injects $STARRY_OVERLAY_DIR into $base_rootfs via debugfs WITHOUT resizing, so the
# per-app image must be grown here first. The overlay carries the mesa/lavapipe closure + its LLVM
# runtime plus the Deno binary (~100 MiB); the stock ~2 GiB image overflows and debugfs silently
# truncates ("Could not allocate block"), surfacing at runtime as "symbol not found". Idempotent.
ROOTFS_SIZE=4G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    command -v resize2fs >/dev/null 2>&1 || { echo "prebuild: resize2fs required (e2fsprogs)" >&2; exit 1; }
    local before after
    before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB (fs resized) for lavapipe + Deno"
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa software Vulkan (lavapipe) + LLVM + the Vulkan loader, all musl for the target arch. `deno`
# (native musl) is added only for x86_64, the sole arch Alpine builds it for. mesa-dev is intentionally
# not installed (it pulls the ~200MB clang-libs closure the runtime does not need).
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers zlib)
case "$arch" in x86_64|aarch64) GPU_PKGS+=(deno) ;; esac

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add WebGPU runtime stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -f "$staging_root/usr/lib/libvulkan_lvp.so" ]] || { echo "prebuild: mesa-vulkan-swrast (lavapipe) not provisioned" >&2; exit 3; }
}

# Stage the carpets + the capability manifest. Deno runs .js/.ts natively, so no tsc, node_modules, or
# build step is needed on-target. The manifest lists exactly the cells whose runtime was provisioned on
# this arch (Deno present => webgpu_js + webgpu_ts); run-webgpu-render.sh gates on that exact set (fail==0 &&
# total==EXPECTED==pass, EXPECTED>=2). On arches with no Deno the manifest is empty and run-webgpu-render.sh
# refuses to emit a pass.
# Scene cells: 4 render scenarios x {js, ts}, each a self-contained Deno program under carpets/<name>/.
# They mirror the wgpu Rust render-scene cells (scene_2dui/3dmodel/anim/codec) one-for-one, staged the same
# way as webgpu_js/webgpu_ts (Deno runs .js/.ts natively, no build).
SCENE_CELLS=(scene_2dui_js scene_2dui_ts scene_3dmodel_js scene_3dmodel_ts scene_anim_js scene_anim_ts scene_codec_js scene_codec_ts)

stage_carpets() {
    local bin="$staging_root/opt/cpu-webgpu-render"
    mkdir -p "$bin/carpets/webgpu_js" "$bin/carpets/webgpu_ts"
    install -Dm0644 "$PROG/carpets/webgpu_js/webgpu_render_js_full_api.js" "$bin/carpets/webgpu_js/webgpu_render_js_full_api.js"
    install -Dm0644 "$PROG/carpets/webgpu_ts/webgpu_render_ts_full_api.ts" "$bin/carpets/webgpu_ts/webgpu_render_ts_full_api.ts"
    for cell in "${SCENE_CELLS[@]}"; do
        local ext="${cell##*_}"
        mkdir -p "$bin/carpets/$cell"
        install -Dm0644 "$PROG/carpets/$cell/$cell.$ext" "$bin/carpets/$cell/$cell.$ext"
    done
    install -Dm0755 "$PROG/run-webgpu-render.sh" "$bin/run-webgpu-render.sh"
    : > "$bin/expected_cells"
    if [[ -x "$staging_root/usr/bin/deno" ]]; then
        echo "webgpu_js" >> "$bin/expected_cells"
        echo "webgpu_ts" >> "$bin/expected_cells"
        for cell in "${SCENE_CELLS[@]}"; do echo "$cell" >> "$bin/expected_cells"; done
        echo "prebuild: Deno provisioned ($("$staging_root/usr/bin/deno" --version 2>/dev/null | head -1 || echo deno)); cells = webgpu_js webgpu_ts ${SCENE_CELLS[*]}"
    else
        echo "prebuild: no native-musl Deno provisioned for $arch yet (x86_64/aarch64 only); manifest empty"
    fi
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the provisioned /usr/lib closure (mesa lavapipe + LLVM + Vulkan loader) and ICD metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    # Deno binary (x86_64); its musl deps live under /usr/lib (already copied above)
    [[ -x "$staging_root/usr/bin/deno" ]] && install -Dm0755 "$staging_root/usr/bin/deno" "$overlay_dir/usr/bin/deno"
    cp -a "$staging_root/opt/cpu-webgpu-render" "$overlay_dir/opt/"
    install -Dm0755 "$PROG/run-webgpu-render.sh" "$overlay_dir/usr/bin/run-webgpu-render.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
stage_carpets
populate_overlay
echo "prebuild: cpu-webgpu-render overlay ready for $arch"
