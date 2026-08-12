#!/usr/bin/env bash
# prebuild.sh - provision the cpu-model-test carpet ("pyte for 3D models") into the per-arch overlay.
#
# The carpet drives self-written OBJ/STL/PLY parsers + a mesh-plane slicer + a barycentric/z-buffer software
# rasterizer + point-cloud stats (all under programs/carpets), plus a vendored single-header cgltf.h for
# glTF/glb, and asserts the output against CLOSED-FORM properties (a unit-cube slice is a square with
# perimeter 4.0 / area 1.0; a sphere-sampled cloud has centroid at the origin and all points at radius r) or
# goldens calibrated host-side with this exact code (bunny.ply count/bbox/centroid/spatial-hash signature;
# suzanne render coverage/depth signature; per-layer slice goldens in render-assets/golden/slice_golden.json).
# Only the parsers/slicer/rasterizer/comparisons and the golden constants are self-written; no heavy mesh lib
# (assimp) is pulled, and glTF is reused via cgltf, not hand-rolled.
#
# The cells need only libc + libm + the vendored cgltf.h - a musl static binary needs no shared 3D library on
# target. So the cells are cross-compiled directly on the host with a musl-cross toolchain (no qemu-user, no
# apk-into-staging). Running the target Alpine gcc under qemu-user-static fails because cc1 cannot posix_spawn
# under qemu-user; the host musl-cross path is the same one the merged cpu-concurrency sibling uses.
#
# The derived closed-form assets (cube in 5 formats + synthetic sphere cloud) are generated host-side with
# tools/gen_goldens.py; the real models (suzanne/benchy/bunny) are staged from the media submodule or a
# render-assets tree when present, and asset-dependent legs honest-skip when absent.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR. Optional: MODEL_ASSET_SRC (host
# path to render-assets/models), PC_ASSET_SRC (host path to render-assets/pointcloud); default to the trees
# found by walking up from the app dir.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"
THIRD_PARTY="$CAR/third_party"

case "$arch" in
    x86_64)      triple="x86_64-linux-musl" ;;
    aarch64)     triple="aarch64-linux-musl" ;;
    riscv64)     triple="riscv64-linux-musl" ;;
    loongarch64) triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# cgltf.h is under third_party/ which the repo .gitignore excludes, so it never ships with the PR. Fetch it
# from the official upstream at a pinned commit and verify byte-identity against the SHA-256 of the copy this
# carpet was calibrated with, so the fetched header is exactly what the goldens were computed against.
CGLTF_COMMIT="de9828bc6419064c302546313ce8ff5eac6cd703"
CGLTF_SHA256="efb169dee911696b5d35fc8e3f7ea0c56d679debc529eba9ca6aa6443ba9d5e9"
CGLTF_URL="https://raw.githubusercontent.com/jkuhlmann/cgltf/${CGLTF_COMMIT}/cgltf.h"

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

ensure_cgltf() {
    mkdir -p "$THIRD_PARTY"
    local dst="$THIRD_PARTY/cgltf.h"
    # A present copy is only trusted if it already matches the pin; otherwise (missing or gitignored-away)
    # re-fetch from the pinned upstream commit.
    if [[ -f "$dst" && "$(sha256_of "$dst")" == "$CGLTF_SHA256" ]]; then
        echo "prebuild: cgltf.h present and matches pin ($CGLTF_SHA256)"
        return 0
    fi
    echo "prebuild: fetching cgltf.h from $CGLTF_URL"
    local tmp; tmp="$(mktemp)"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$CGLTF_URL" -o "$tmp" || { echo "prebuild: cgltf.h fetch failed (curl)" >&2; rm -f "$tmp"; exit 6; }
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$tmp" "$CGLTF_URL" || { echo "prebuild: cgltf.h fetch failed (wget)" >&2; rm -f "$tmp"; exit 6; }
    else
        echo "prebuild: neither curl nor wget available to fetch cgltf.h" >&2; rm -f "$tmp"; exit 6
    fi
    local got; got="$(sha256_of "$tmp")"
    if [[ "$got" != "$CGLTF_SHA256" ]]; then
        echo "prebuild: cgltf.h SHA-256 mismatch - expected $CGLTF_SHA256 got $got" >&2
        rm -f "$tmp"; exit 6
    fi
    mv -f "$tmp" "$dst"
    echo "prebuild: cgltf.h fetched + verified ($CGLTF_SHA256, commit $CGLTF_COMMIT)"
}

# Resolve a host musl-cross compiler for the target triple: the standard cross-gcc on PATH, then the
# conventional /opt/<triple>-cross install prefix, then `zig cc -target <triple>` as a portable fallback, and
# for a native build musl-gcc. This mirrors the merged cpu-concurrency sibling.
resolve_cc() {
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
        exit 3
    fi
    echo "prebuild: cross-compiling for $arch with ${CC[*]}"
}

# -no-pie: emit a plain static (non-PIE) binary. Without it the riscv64/aarch64/x86_64 musl toolchains default
# to static-PIE, whose dynamic relocations complicate the static loader; -no-pie is correct on all arches.
CFLAGS=(-O2 -std=c11 -static -no-pie)
CELLS=(model_parse model_slice model_render model_pointcloud model_realassets)

# Each cell is a standalone C program including model_common.h + the self-written parsers/slicer/rasterizer
# (and cgltf for the glb leg). A compile failure is a genuine breakage. Static link so target needs no shared
# 3D lib.
compile_cells() {
    local bin="$1" cell
    for cell in "${CELLS[@]}"; do
        echo "prebuild: cross-compile $cell for $arch (self-written parsers/slicer/rasterizer + vendored cgltf; self-written goldens)"
        "${CC[@]}" "${CFLAGS[@]}" -I"$CAR" "$CAR/$cell.c" -o "$bin/$cell" -lm
        [[ -x "$bin/$cell" ]] || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
    done
}

find_models_src() {
    local s="${MODEL_ASSET_SRC:-}"
    # Preferred source: the per-app `assets` git submodule (same models/ layout as render-assets).
    # On a fresh CI checkout the gitlink dir exists but is empty until inited, and the meshes arrive as
    # LFS pointers - init + sparse-pull the models/ subdir so the marker materializes with real bytes.
    if [[ -z "$s" && -d "$app_dir/assets" ]]; then
        if [[ ! -e "$app_dir/assets/models/suzanne.stl" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        if command -v git >/dev/null 2>&1 && git -C "$app_dir/assets" lfs env >/dev/null 2>&1; then
            git -C "$app_dir/assets" lfs pull --include="models/*,pointcloud/*,golden/*" >/dev/null 2>&1 || true
        fi
        [[ -f "$app_dir/assets/models/suzanne.stl" ]] && s="$app_dir/assets/models"
    fi
    if [[ -z "$s" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -f "$d/render-assets/models/suzanne.stl" ]]; then s="$d/render-assets/models"; break; fi
        done
    fi
    echo "$s"
}
find_pc_src() {
    local s="${PC_ASSET_SRC:-}"
    # Preferred source: the per-app `assets` git submodule (same pointcloud/ layout as render-assets).
    # The submodule is inited + pulled by find_models_src; bunny.ply is an LFS blob pulled in that step.
    if [[ -z "$s" && -f "$app_dir/assets/pointcloud/bunny.ply" ]]; then s="$app_dir/assets/pointcloud"; fi
    if [[ -z "$s" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -f "$d/render-assets/pointcloud/bunny.ply" ]]; then s="$d/render-assets/pointcloud"; break; fi
        done
    fi
    echo "$s"
}

# A Git LFS pointer is a small text file whose first line is "version https://git-lfs...". Without git-lfs
# the submodule materializes pointers, not the real meshes; a pointer opens fine but is not a valid model,
# so a cell that fopen()s it would parse garbage and FAIL instead of honest-skipping. Detect the pointer and
# treat it as "asset not materialized" so the asset-gated legs honest-skip rather than assert on a text blob.
is_lfs_pointer() {
    local p="$1" first
    [[ -f "$p" ]] || return 1
    IFS= read -r first < "$p" 2>/dev/null || return 1
    [[ "$first" == version\ https://git-lfs* ]]
}

# Copy one real asset into the overlay only if it materialized. A missing file honest-skips; a staged LFS
# pointer is refused (never copied) with a loud note pointing at the git-lfs fix, so no cell ever asserts
# against pointer text.
stage_real() {
    local src="$1" dst_dir="$2" name; name="$(basename "$src")"
    if [[ ! -f "$src" ]]; then
        echo "prebuild: model $name absent (leg honest-skips)"
        return 0
    fi
    if is_lfs_pointer "$src"; then
        echo "prebuild: model $name is an unresolved Git-LFS pointer, not the real asset - refusing to stage (install git-lfs and 'git lfs pull'); leg honest-skips" >&2
        return 0
    fi
    cp -a "$src" "$dst_dir/" && echo "prebuild: staged $name"
}

# Stage the real models + point clouds under /opt/cpu-model-test/assets, then generate the derived
# closed-form assets (cube in OBJ/STL-ascii/STL-bin/PLY-ascii/PLY-bin + synthetic sphere cloud) with
# tools/gen_goldens.py. A missing render-assets tree never fails the gate (the closed-form legs still run);
# present assets are asserted against the calibrated goldens baked into the cells.
stage_assets() {
    local bin="$1" models pc
    mkdir -p "$bin/assets"
    models="$(find_models_src || true)"
    if [[ -n "${models:-}" && -d "$models" ]]; then
        echo "prebuild: staging models from $models -> /opt/cpu-model-test/assets"
        for f in suzanne.obj suzanne.stl suzanne.glb benchy.stl; do
            stage_real "$models/$f" "$bin/assets"
        done
    else
        echo "prebuild: render-assets/models not found (set MODEL_ASSET_SRC) - real-model legs honest-skip"
    fi
    pc="$(find_pc_src || true)"
    if [[ -n "${pc:-}" ]] && [[ -f "$pc/bunny.ply" ]] && ! is_lfs_pointer "$pc/bunny.ply"; then
        cp -a "$pc/bunny.ply" "$bin/assets/" && echo "prebuild: staged bunny.ply"
        if [[ -f "$pc/bunny_scan000.ply" ]] && ! is_lfs_pointer "$pc/bunny_scan000.ply"; then
            cp -a "$pc/bunny_scan000.ply" "$bin/assets/"
        fi
    elif [[ -n "${pc:-}" ]] && [[ -f "$pc/bunny.ply" ]]; then
        echo "prebuild: pointcloud/bunny.ply is an unresolved Git-LFS pointer - refusing to stage (install git-lfs and 'git lfs pull'); bunny leg honest-skips" >&2
    else
        echo "prebuild: render-assets/pointcloud/bunny.ply absent - bunny leg honest-skips"
    fi

    # Generate the derived closed-form assets (cube x5 formats + sphere cloud). The generator also recomputes
    # the bunny golden as a cross-check but the C cells carry the pinned constant.
    echo "prebuild: generating derived closed-form assets (cube 5 formats + sphere cloud) via tools/gen_goldens.py"
    command -v python3 >/dev/null 2>&1 || { echo "prebuild: python3 required to generate derived cube/sphere assets" >&2; exit 5; }
    python3 "$app_dir/tools/gen_goldens.py" "$bin/assets" "$bin/assets/bunny.ply" >/dev/null 2>&1 \
        || python3 "$app_dir/tools/gen_goldens.py" "$bin/assets" >/dev/null 2>&1 \
        || { echo "prebuild: derived-asset generation failed" >&2; exit 5; }
    for f in cube.obj cube.stl cube_ascii.stl cube.ply cube_bin.ply sphere_pc.ply; do
        [[ -f "$bin/assets/$f" ]] || { echo "prebuild: derived asset $f missing after generation" >&2; exit 5; }
    done
    local n; n=$(ls "$bin/assets" 2>/dev/null | wc -l)
    echo "prebuild: staged $n asset files (real + derived closed-form)"
}

compile_carpets() {
    local bin="$overlay_dir/opt/cpu-model-test"; mkdir -p "$bin"
    compile_cells "$bin"
    stage_assets "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

populate_overlay() {
    local bin="$overlay_dir/opt/cpu-model-test"
    : > "$bin/expected_cells"
    for c in "${CELLS[@]}"; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/bin"
    ln -sf /opt/cpu-model-test/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$bin/assets" 2>/dev/null | cut -f1) assets)"
}

ensure_cgltf
resolve_cc
compile_carpets
populate_overlay
echo "prebuild: cpu-model-test overlay ready for $arch"
