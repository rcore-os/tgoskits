#!/usr/bin/env bash
# prebuild.sh - cross-compile the cpu-subtitle-test carpet ("pyte for subtitles") into the per-arch overlay.
#
# Each cell is a standalone C11 program that includes the self-written SubRip (.srt) / WebVTT (.vtt) /
# Advanced SubStation Alpha (.ass) parsers + a cross-format timing converter (all under programs/carpets).
# The cells link only against libc - no heavy subtitle lib (libass) is pulled, the parsers are self-written -
# so there is nothing to stage into a rootfs but the compiled binaries plus the real subtitle assets.
#
# The compiler is resolved from the standard musl-cross toolchain names on PATH, then the conventional
# /opt/<triple>-cross install prefix, then `zig cc -target <triple>`, then (x86_64 only) musl-gcc. Cells are
# cross-compiled directly on the host - the previous flow ran the TARGET Alpine gcc under qemu-user, whose
# cc1 cannot exec under qemu-user (`cc1: posix_spawn`), so no cell ever compiled. No pinned URLs, no qemu-user,
# no apk-into-staging.
#
# Env from the app runner: STARRY_ARCH (required), STARRY_OVERLAY_DIR (required), STARRY_APP_DIR.
# Optional: SUBTITLE_ASSET_SRC (host path to the subtitles/ dir); default to the assets submodule / the
# render-assets tree found by walking up from the app dir.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"

case "$arch" in
    x86_64)      triple="x86_64-linux-musl" ;;
    aarch64)     triple="aarch64-linux-musl" ;;
    riscv64)     triple="riscv64-linux-musl" ;;
    loongarch64) triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Resolve the host cross toolchain once. Same order as the merged sibling carpets:
# standard cross-gcc on PATH -> conventional /opt/<triple>-cross prefix -> zig cc -> (x86_64) musl-gcc.
if command -v "${triple}-gcc" >/dev/null 2>&1; then
    CC=("${triple}-gcc")
elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then
    CC=("/opt/${triple}-cross/bin/${triple}-gcc")
elif command -v zig >/dev/null 2>&1; then
    CC=(zig cc -target "$triple")
elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then
    CC=(musl-gcc)
else
    echo "prebuild: no musl cross toolchain for $triple (tried ${triple}-gcc, /opt/${triple}-cross, zig cc, musl-gcc)" >&2
    exit 1
fi

# -static: the cells bundle the parsers, so a musl static binary needs no shared subtitle library on target.
CFLAGS=(-O2 -std=c11 -static -Wno-unused-function -I"$CAR")

CELLS=(subtitle_srt subtitle_ass subtitle_vtt subtitle_convert subtitle_realassets)

# A compile failure is a genuine breakage of the cell - not a skip.
compile_cells() {
    local bin="$1" cell
    for cell in "${CELLS[@]}"; do
        echo "prebuild: cross-compile $cell for $arch via ${CC[*]} (self-written srt/vtt/ass parsers + self-written goldens)"
        "${CC[@]}" "${CFLAGS[@]}" "$CAR/$cell.c" -o "$bin/$cell"
        [[ -x "$bin/$cell" ]] || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
    done
}

find_subs_src() {
    local s="${SUBTITLE_ASSET_SRC:-}"
    # Preferred source: the per-app `assets` git submodule (subtitles/ layout). On a fresh CI checkout the
    # gitlink dir exists but is empty until inited; init + update it. tashouheng.srt / badapple.ass are plain
    # git blobs (not LFS), so they materialize on the plain checkout.
    if [[ -z "$s" && -d "$app_dir/assets" ]]; then
        if [[ ! -f "$app_dir/assets/subtitles/tashouheng.srt" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        [[ -f "$app_dir/assets/subtitles/tashouheng.srt" ]] && s="$app_dir/assets/subtitles"
    fi
    if [[ -z "$s" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -f "$d/render-assets/subtitles/tashouheng.srt" ]]; then s="$d/render-assets/subtitles"; break; fi
        done
    fi
    echo "$s"
}

# Stage the real subtitle files under /opt/cpu-subtitle-test/assets. These are plain blobs in the media
# submodule; a missing/empty submodule is a hard failure here so the real-asset leg cannot vacuously
# honest-skip on target - the overlay always ships both real files or the prebuild aborts.
stage_assets() {
    local bin="$1" subs
    mkdir -p "$bin/assets"
    subs="$(find_subs_src || true)"
    [[ -n "${subs:-}" && -d "$subs" ]] \
        || { echo "prebuild: subtitles source not found - run 'git submodule update --init assets' or set SUBTITLE_ASSET_SRC" >&2; exit 5; }
    echo "prebuild: staging subtitles from $subs -> /opt/cpu-subtitle-test/assets"
    for f in tashouheng.srt badapple.ass; do
        cp -a "$subs/$f" "$bin/assets/" \
            || { echo "prebuild: real subtitle asset $f missing under $subs" >&2; exit 5; }
    done
    echo "prebuild: staged $(ls "$bin/assets" | wc -l) subtitle asset files"
}

populate_overlay() {
    local bin="$overlay_dir/opt/cpu-subtitle-test"
    mkdir -p "$bin"
    compile_cells "$bin"
    stage_assets "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"

    # Capability manifest: run_all.sh gates on it (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor).
    : > "$bin/expected_cells"
    for c in "${CELLS[@]}"; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"

    mkdir -p "$overlay_dir/usr/bin"
    ln -sf /opt/cpu-subtitle-test/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
}

populate_overlay
echo "prebuild: cpu-subtitle-test overlay ready for $arch ($triple)"
