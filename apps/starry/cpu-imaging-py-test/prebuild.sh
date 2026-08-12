#!/usr/bin/env bash
# prebuild.sh - provision the cpu-imaging-py-test carpet into the per-arch Alpine rootfs.
#
# An industrial-grade Python imaging test carpet covering Pillow (PIL) + imageio + scikit-image. Each cell
# drives a REAL imaging library on KNOWN, fixed inputs and asserts CLOSED-FORM / numpy goldens computed by
# hand (PIL's L24 601-2 luma, bilinear interpolation at derived source coords, analytic drawn masks,
# impulse-response kernels, byte-exact PNG/BMP/PPM/TIFF/GIF round-trips + JPEG PSNR, skimage's BT.709 luma,
# a Sobel ramp's constant gradient, Otsu on a bimodal field, morphology on a known pattern, regionprops on
# known blobs, cross-library decode agreement). "import PIL" is NOT a test - every leg checks a value.
#
# Libraries are REUSED (never reimplemented):
#   Pillow (PIL)   - Alpine `py3-pillow` (musl) - `from PIL import Image`.
#   numpy          - Alpine `py3-numpy` (the golden reference lib).
#   scipy          - Alpine `py3-scipy` (scikit-image runtime dependency).
#   imageio        - pip wheel staged into site-packages (py3-imageio may not exist on Alpine).
#   scikit-image   - pip wheel staged into site-packages (+ its networkx/tifffile/lazy-loader/pillow deps).
#
# Network note (repo owner): apt is broken (proxy) and SSH blocks - fetch via HTTP. apk pulls the Alpine
# musl wheels for py3-pillow/numpy/scipy; the pure-Python / musl wheels for imageio + scikit-image are
# fetched with `pip download` (HTTP to PyPI) and installed with `pip install --no-index --find-links`.
# imageio + scikit-image must run on all four arches per the four-dimension bar: if a wheel cannot be
# resolved / staged / imported for an arch, prebuild HARD-FAILS (a "不支持" to surface) rather than dropping
# the cell and letting the manifest self-shrink.
#
# Portable model (same as the opencv/subtitle/model carpets): extract the base Alpine rootfs, apk add the
# Python + Pillow/numpy/scipy stack for the TARGET arch via qemu-user, pip-download the imageio +
# scikit-image wheels and install them into the staged site-packages, stage the CPython + all site-packages
# into the overlay, and write a FIXED expected_cells manifest (all four cells: imaging_pil, imaging_imageio,
# imaging_skimage, imaging_realassets) that run_all.sh gates on (fail==0 && total==EXPECTED==pass). EXPECTED
# is constant across arches so the gate cannot be met by a shrunk manifest. imaging_realassets iterates the
# shared media format zoo (the `assets` git submodule Lfan-ke/hw4os-s5d1t2@media, images/ = fmt.png/bmp/
# ppm/pgm/jpg/webp + real rasters), inited + LFS-pulled here and staged to $INSTALL_DIR/assets, plus the
# pinned programs/sample_red.png known-content golden. No ffmpeg/mp4 leg: the imageio_ffmpeg binary is not
# available for all four arches, so that leg was dropped rather than counted as a skip-as-pass.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"
INSTALL_DIR=/opt/cpu-imaging-py-test
CELLS="imaging_pil imaging_imageio imaging_skimage imaging_realassets"

# pip wheels not on Alpine as py3-* packages; fetched over HTTP from PyPI and staged into site-packages.
PIP_PKGS=(imageio scikit-image)

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     pip_plat="musllinux_1_2_aarch64" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     pip_plat="musllinux_1_2_riscv64" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      pip_plat="musllinux_1_2_x86_64" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; pip_plat="musllinux_1_2_loongarch64" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    command -v pip3 >/dev/null 2>&1 || command -v pip >/dev/null 2>&1 || missing+=(python3-pip)
    if [[ ${#missing[@]} -gt 0 ]]; then
        command -v apt-get >/dev/null 2>&1 && apt-get update && apt-get install -y --no-install-recommends "${missing[@]}" \
            || { echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1; }
    fi
}

# scikit-image + scipy + a full CPython need generous room.
ROOTFS_SIZE=6G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for the imaging stack"
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

# Pillow (py3-pillow), numpy (py3-numpy, the golden lib), scipy (py3-scipy, a scikit-image runtime dep),
# python3 + pip. No ffmpeg: the imageio mp4 leg was dropped (its imageio_ffmpeg binary is not available for
# all four arches), so only still-image codecs are exercised and no video toolchain is provisioned.
PKGS=(musl python3 py3-pip py3-pillow py3-numpy py3-scipy)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add imaging Python stack (${PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${PKGS[@]}"
    [[ -x "$staging_root/usr/bin/python3" ]] \
        || { echo "prebuild: python3 not provisioned for $arch" >&2; exit 3; }
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -c 'import PIL, numpy, scipy' \
        || { echo "prebuild: Pillow/numpy/scipy not importable on target - apk stack incomplete" >&2; exit 3; }
}

# Fetch the imageio + scikit-image wheels (and their pure-python deps) over HTTP from PyPI, targeting the
# staged CPython's version + the target musl platform, then install them offline into site-packages. Pure
# python deps (lazy-loader, networkx, tifffile, imageio) come as any-platform wheels; scikit-image ships a
# musllinux wheel per arch. If pip cannot resolve a wheel for an arch, prebuild hard-fails (see below).
WHEELS="$app_dir/.wheels-$arch"
provision_pip_wheels() {
    local pyver pymm
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    [[ -n "$pyver" ]] || { echo "prebuild: no python3.x under staging" >&2; exit 5; }
    pymm="${pyver#python}"   # e.g. 3.12
    local sitepkgs="$staging_root/usr/lib/$pyver/site-packages"
    mkdir -p "$sitepkgs" "$WHEELS"
    local pip; pip="$(command -v pip3 || command -v pip)"
    echo "prebuild: pip download ${PIP_PKGS[*]} for cp${pymm//./} / $pip_plat (HTTP PyPI)..."
    # download target-arch wheels; --only-binary avoids sdists (no target compiler in this path). imageio and
    # scikit-image must run on all four arches per the four-dimension bar, so an unresolved wheel is a hard
    # error (a genuine "不支持" to surface), not a silent cell drop.
    if ! "$pip" download "${PIP_PKGS[@]}" \
            --dest "$WHEELS" --only-binary=:all: \
            --python-version "$pymm" --implementation cp --abi "cp${pymm//./}" \
            --platform "$pip_plat" --platform any 2>"$WHEELS/download.log"; then
        echo "prebuild: pip could not resolve imageio/scikit-image wheels for $pip_plat (cp${pymm//./})" >&2
        echo "prebuild: see $WHEELS/download.log - imageio/scikit-image are required for all four arches" >&2
        exit 5
    fi
    echo "prebuild: installing wheels offline into $sitepkgs"
    "$pip" install --no-index --find-links "$WHEELS" --target "$sitepkgs" --upgrade \
        --no-deps "$WHEELS"/*.whl 2>"$WHEELS/install.log" \
        || { echo "prebuild: offline wheel install failed (see $WHEELS/install.log)" >&2; exit 5; }
}

# The Python cells are copied verbatim; they import PIL/imageio/skimage + numpy on target.
stage_python_cells() {
    local bin="$1"
    mkdir -p "$bin/py"
    cp "$CAR/py/"*.py "$bin/py/"
}

# Locate the shared media format-zoo images. Preferred source is the per-app `assets` git submodule
# (Lfan-ke/hw4os-s5d1t2, branch media), which carries images/ (fmt.png/bmp/ppm/pgm/jpg/webp + real
# rasters). On a fresh checkout the gitlink dir exists but is empty until inited, and the rasters arrive
# as LFS pointers - init + LFS-pull images/ so real bytes materialize. Falls back to IMAGE_ASSET_SRC or a
# render-assets/images tree found by walking up from the app dir. An empty corpus is tolerated because the
# pinned sample_red.png golden (staged below) still gives realassets a real content assertion to run.
find_image_src() {
    local src="${IMAGE_ASSET_SRC:-}"
    if [[ -z "$src" && -d "$app_dir/assets" ]]; then
        if [[ ! -e "$app_dir/assets/images/fmt_ref.png" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        if command -v git >/dev/null 2>&1 && git -C "$app_dir/assets" lfs env >/dev/null 2>&1; then
            git -C "$app_dir/assets" lfs pull --include="images/*" >/dev/null 2>&1 || true
        fi
        [[ -d "$app_dir/assets/images" ]] && src="$app_dir/assets/images"
    fi
    if [[ -z "$src" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -d "$d/render-assets/images" ]]; then src="$d/render-assets/images"; break; fi
        done
    fi
    echo "$src"
}

# Stage the media format zoo into $bin/assets so imaging_realassets iterates it on-target (run_all.sh sets
# ASSET_DIR=$bin/assets). Only formats all three libs decode are copied (png/bmp/ppm/pgm/tiff/jpg/webp);
# the pinned deterministic red sample (programs/sample_red.png) is staged to $bin/sample_red.png for the
# known-content leg. A missing corpus is not fatal - the pinned sample_red.png still drives a real content
# assertion, so realassets never skip-passes.
stage_assets() {
    local bin="$1" src f n=0
    mkdir -p "$bin/assets"
    src="$(find_image_src)"
    if [[ -n "$src" && -d "$src" ]]; then
        echo "prebuild: staging media format zoo from $src -> $INSTALL_DIR/assets"
        for f in "$src"/*.png "$src"/*.bmp "$src"/*.ppm "$src"/*.pgm \
                 "$src"/*.tiff "$src"/*.tif "$src"/*.jpg "$src"/*.jpeg "$src"/*.webp; do
            [[ -f "$f" ]] || continue
            cp -a "$f" "$bin/assets/" && n=$((n+1))
        done
        echo "prebuild: staged $n corpus images"
    else
        echo "prebuild: media images/ not found (submodule not inited, no IMAGE_ASSET_SRC) - realassets runs on the pinned sample only" >&2
    fi
    # pinned deterministic red case (committed under programs/); staged next to the cells for the golden leg.
    # It is committed, so its absence is a broken checkout, not an optional asset - hard-fail rather than let
    # realassets lose its known-content assertion.
    if [[ -f "$app_dir/programs/sample_red.png" ]]; then
        cp -a "$app_dir/programs/sample_red.png" "$bin/sample_red.png"
        echo "prebuild: staged pinned sample_red.png (red-dominant golden)"
    else
        echo "prebuild: programs/sample_red.png absent - committed pinned golden missing, checkout is broken" >&2
        exit 7
    fi
}

# Stage the CPython interpreter + stdlib + site-packages (PIL/numpy/scipy from apk, imageio/skimage from
# pip) + the shared-lib closure into the overlay so the cells run on target.
stage_runtime() {
    echo "prebuild: staging CPython + Pillow/numpy/scipy/imageio/scikit-image runtime into overlay"
    (cd "$staging_root" && find usr/lib -maxdepth 1 -name '*.so*' -print) | while read -r rel; do
        mkdir -p "$overlay_dir/$(dirname "$rel")"; cp -a "$staging_root/$rel" "$overlay_dir/$rel" 2>/dev/null || true
    done
    local pyver
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    [[ -n "$pyver" ]] || { echo "prebuild: no python3.x under staging" >&2; exit 5; }
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/bin"
    cp -a "$staging_root/usr/lib/$pyver" "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/bin/python3" "$overlay_dir/usr/bin/" 2>/dev/null || true
    [[ -e "$staging_root/usr/bin/$pyver" ]] && cp -a "$staging_root/usr/bin/$pyver" "$overlay_dir/usr/bin/" || true
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true
}

compile_carpets() {
    local bin="$staging_root$INSTALL_DIR"; mkdir -p "$bin/assets"
    stage_python_cells "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    stage_assets "$bin"
    stage_runtime
}

# Verify a module imports on target through the staged site-packages. Used as a hard gate, not a drop
# condition: PIL/numpy come from apk and imageio/scikit-image from the staged pip wheels - all four cells
# must run on all four arches, so a failed import aborts the build rather than shrinking the manifest.
target_can_import() {
    local mod="$1" pyver sitepkgs
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    sitepkgs="/usr/lib/$pyver/site-packages"
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        PYTHONPATH="$staging_root$sitepkgs" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -c "import $mod" >/dev/null 2>&1
}

populate_overlay() {
    local bin="$staging_root$INSTALL_DIR"
    # imageio + scikit-image must import on target - if a wheel staged but is unusable for this arch, that is
    # a "不支持" to surface, not a cell to drop. A soft manifest would let EXPECTED shrink with the coverage
    # and still print TEST PASSED, so probe here and hard-fail instead.
    target_can_import imageio \
        || { echo "prebuild: imageio not importable on $arch through staged site-packages" >&2; exit 6; }
    target_can_import skimage \
        || { echo "prebuild: scikit-image not importable on $arch through staged site-packages" >&2; exit 6; }
    # Fixed manifest: all four cells always, so EXPECTED is constant across arches and the three-gate cannot
    # be satisfied by a self-shrunk manifest.
    printf 'py/imaging_pil\npy/imaging_imageio\npy/imaging_skimage\npy/imaging_realassets\n' > "$bin/expected_cells"
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/opt"
    cp -a "$staging_root$INSTALL_DIR" "$overlay_dir/opt/"
    mkdir -p "$overlay_dir/usr/bin"
    ln -sf "$INSTALL_DIR/run_all.sh" "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
provision_pip_wheels
compile_carpets
populate_overlay
echo "prebuild: cpu-imaging-py-test overlay ready for $arch"
