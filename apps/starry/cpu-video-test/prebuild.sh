#!/usr/bin/env bash
# prebuild.sh - provision the cpu-video-test carpet ("pyte for video") into the per-arch Alpine rootfs.
#
# The carpet decodes video to raw pixels + audio to raw PCM with the `ffmpeg` CLI and asserts in the
# PIXEL / SIGNAL / TIMING domains against analytically-known or golden references. Runtime dependency
# is just the `ffmpeg` + `ffprobe` CLI (Alpine musl `ffmpeg` package, which ships libavcodec/libavformat
# with h264/hevc/vp9/ffv1/mpeg2 + aac/flac/opus/vorbis) plus a C toolchain to build the cells. No codec
# or DSP library is linked - the cells embed a self-written raw-frame reader, PSNR/SSIM, an FFT, a
# SHA-256 and the reference math, and shell out to `ffmpeg`/`ffprobe` only to decode/encode/probe.
#
# Portable model (same as the audio/render/compute carpets): extract the base Alpine rootfs, `apk add`
# ffmpeg + ffprobe for the TARGET arch via qemu-user (apk runs fine under qemu-user; only gcc's cc1
# cannot), cross-compile each cell with a HOST musl-cross toolchain, stage the binaries + run_all.sh +
# (optionally) the real-media assets under /opt/cpu-video-test/assets, and write a capability manifest
# that run_all.sh gates on (fail==0 && total==EXPECTED==pass, floor>=1).
#
# The cells link only libc/libm and decode media by shelling out to the on-target ffmpeg/ffprobe CLI at
# runtime, so they cross-compile directly on the host. The staging gcc's cc1 cannot exec under qemu-user
# (posix_spawn fails), so cells are built with the host cross-gcc, not the target Alpine gcc.
#
# The synthetic cells (video_codec, video_meta, video_avsync) are the guaranteed gate - they need only
# ffmpeg + libc and generate their own known clips. video_frames + video_realassets always build; at
# runtime video_realassets honest-skips if $ASSET_DIR is absent and video_frames still runs its fully
# synthetic testsrc leg, so a missing submodule never fails the gate but present assets are asserted.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR,
# STARRY_APP_DIR. Optional: VIDEO_ASSET_SRC (host path to the render-assets tree to stage into the
# image; defaults to <repo>/render-assets if present).
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"

CELLS=(video_frames video_codec video_meta video_avsync video_realassets)

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     triple="aarch64-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     triple="riscv64-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      triple="x86_64-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Resolve a HOST musl-cross compiler for $triple: standard cross-gcc on PATH, then the conventional
# /opt/<triple>-cross install prefix, then `zig cc -target <triple>` as a portable fallback, and for a
# native x86_64 build also musl-gcc. The cells must be compiled on the host - the target Alpine gcc's
# cc1 cannot exec under qemu-user (posix_spawn fails).
resolve_cc() {
    if command -v "${triple}-gcc" >/dev/null 2>&1; then
        HOST_CC=("${triple}-gcc")
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then
        HOST_CC=("/opt/${triple}-cross/bin/${triple}-gcc")
    elif command -v zig >/dev/null 2>&1; then
        HOST_CC=(zig cc -target "$triple")
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then
        HOST_CC=(musl-gcc)
    else
        echo "prebuild: no host musl cross toolchain for $triple (tried ${triple}-gcc, /opt/${triple}-cross, zig cc, musl-gcc)" >&2
        exit 1
    fi
}

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    if [[ ${#missing[@]} -gt 0 ]]; then
        command -v apt-get >/dev/null 2>&1 && apt-get update && apt-get install -y --no-install-recommends "${missing[@]}" \
            || { echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1; }
    fi
}

ROOTFS_SIZE=4G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    command -v resize2fs >/dev/null 2>&1 || { echo "prebuild: resize2fs required (e2fsprogs)" >&2; exit 1; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for ffmpeg closure + assets"
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

# Runtime closure only: the ffmpeg + ffprobe CLI + shared libs (h264/hevc/vp9/ffv1/mpeg2 +
# aac/flac/opus/vorbis via libavcodec). No build-base - cells are cross-compiled on the host.
PKGS=(musl ffmpeg)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add video runtime (${PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${PKGS[@]}"
    [[ -x "$staging_root/usr/bin/ffmpeg" ]] \
        || { echo "prebuild: ffmpeg CLI not provisioned for $arch" >&2; exit 3; }
    [[ -x "$staging_root/usr/bin/ffprobe" ]] \
        || { echo "prebuild: ffprobe CLI not provisioned for $arch" >&2; exit 3; }
}

# Each cell is a standalone C program including video_common.h; it links only libc/libm and shells out to
# the on-target ffmpeg/ffprobe CLI to decode. A compile failure is a genuine breakage.
compile_cells() {
    local bin="$1"
    local cell
    for cell in "${CELLS[@]}"; do
        echo "prebuild: host cross-compile $cell for $arch ($triple; self-written PSNR/SSIM/FFT/SHA-256, ffmpeg CLI decode)"
        "${HOST_CC[@]}" -O2 -std=c11 -I"$CAR" "$CAR/$cell.c" -o "$bin/$cell" -lm
        [[ -x "$bin/$cell" ]] || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
    done
}

# Stage the real-media assets (video/badapple_frames, video/badapple_clips, golden tsvs) into the image
# under /opt/cpu-video-test/assets so video_frames + video_realassets assert against them on-target.
# On-target these normally ride a git submodule; staging the extracted tree here keeps the carpet
# self-checking. Best-effort: if no asset source is found, both real-asset legs honest-skip at runtime.
stage_assets() {
    local bin="$1"
    local src="${VIDEO_ASSET_SRC:-}"
    # 1. Preferred source: the per-app `assets` git submodule (same golden/ video/ layout as render-assets).
    #    On a fresh CI checkout the gitlink dir exists but is empty until inited, and its media files arrive
    #    as LFS pointers - init + sparse-pull only the subdirs this carpet reads.
    if [[ -z "$src" && -d "$app_dir/assets" ]]; then
        if [[ ! -f "$app_dir/assets/golden/badapple_clips_firstframe.tsv" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        if command -v git >/dev/null 2>&1 && git -C "$app_dir/assets" lfs env >/dev/null 2>&1; then
            git -C "$app_dir/assets" lfs pull --include="video/*,golden/*" >/dev/null 2>&1 || true
        fi
        [[ -f "$app_dir/assets/golden/badapple_clips_firstframe.tsv" ]] && src="$app_dir/assets"
    fi
    # 2. Fallback: walk up from the app dir looking for a checked-out render-assets tree (dev machines).
    if [[ -z "$src" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -f "$d/render-assets/golden/badapple_clips_firstframe.tsv" ]]; then src="$d/render-assets"; break; fi
        done
    fi
    if [[ -n "$src" && -f "$src/golden/badapple_clips_firstframe.tsv" ]]; then
        echo "prebuild: staging real-media assets from $src -> /opt/cpu-video-test/assets"
        mkdir -p "$bin/assets/video" "$bin/assets/golden"
        [[ -d "$src/video/badapple_frames" ]] && cp -a "$src/video/badapple_frames" "$bin/assets/video/" || true
        [[ -d "$src/video/badapple_clips"  ]] && cp -a "$src/video/badapple_clips"  "$bin/assets/video/" || true
        cp -a "$src/golden/badapple_clips_firstframe.tsv" "$bin/assets/golden/" 2>/dev/null || true
        cp -a "$src/golden/badapple_clips_t2.tsv"         "$bin/assets/golden/" 2>/dev/null || true
        cp -a "$src/golden/badapple_frames.tsv"           "$bin/assets/golden/" 2>/dev/null || true
        echo "prebuild: staged $(find "$bin/assets" -type f 2>/dev/null | wc -l) asset files"
    else
        echo "prebuild: no render-assets tree found - video_frames/video_realassets will honest-skip on-target"
    fi
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-video-test"; mkdir -p "$bin"
    compile_cells "$bin"
    stage_assets "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

populate_overlay() {
    local bin="$staging_root/opt/cpu-video-test"
    : > "$bin/expected_cells"
    for c in "${CELLS[@]}"; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/bin" "$overlay_dir/opt"
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root"/usr/bin/ffmpeg  "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root"/usr/bin/ffprobe "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-video-test" "$overlay_dir/opt/"
    ln -sf /opt/cpu-video-test/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

resolve_cc
ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
echo "prebuild: cpu-video-test overlay ready for $arch"
