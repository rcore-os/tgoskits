#!/usr/bin/env bash
# prebuild.sh - provision the cpu-image-test carpet ("pyte for images") into the per-arch overlay.
#
# The carpet drives real image decoders/rasterizers - stb_image (png/bmp/tga/jpg/ppm/pgm/gif decode),
# stb_image_write (png/bmp/tga/jpg encode) and nanosvg + nanosvgrast (SVG parse + rasterize), all pinned
# single-header libs under programs/carpets/third_party - and asserts the output BYTE-EXACT (per-pixel
# SHA-256 / closed-form pixel regions) or PSNR-bounded (lossy) against goldens calibrated host-side with
# those exact libraries. Only the comparison logic (per-pixel diff, PSNR, SHA-256) and the golden constants
# are self-written; no PNG/JPEG/SVG codec is reimplemented - the point is to TEST stb/nanosvg.
#
# Runtime dependency is NONE beyond libc + libm: the cells statically bundle the single-header libs, so a
# musl binary needs no shared image library on target. The cells are cross-compiled on the HOST with a
# musl-cross toolchain (native speed) - the target gcc is never run under qemu-user, because gcc spawns
# cc1 via posix_spawn which qemu-user cannot exec. GIF is palette-quantized (stb has no encoder), so
# prebuild stages a deterministic 4-colour palette GIF (pal.gif) written byte-for-byte here.
#
# The four single-header libs are gitignored (repo-root .gitignore `third_party/`), so they never ship in
# the PR tree. prebuild fetches them from their pinned upstream commits (nothings/stb, memononen/nanosvg)
# and verifies each against a SHA-256 pinned to the exact bytes the goldens were calibrated against; a
# mismatch or fetch failure is a hard error so a drifted header cannot silently change decode output.
#
# The image assets are a git submodule staged onto the overlay; prebuild hard-fails if a required asset is
# absent, and the real-asset cells hard-fail on-target if a required file is missing. The manifest gate in
# run_all.sh requires fail==0 && total==EXPECTED==pass with EXPECTED>=1.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_OVERLAY_DIR, STARRY_APP_DIR. Optional:
# IMAGE_ASSET_SRC (host path to render-assets/images; defaults to the tree found by walking up from the app
# dir).
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"

case "$arch" in
    x86_64)      triple="x86_64-linux-musl" ;;
    aarch64)     triple="aarch64-linux-musl" ;;
    riscv64)     triple="riscv64-linux-musl" ;;
    loongarch64) triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Resolve a musl-cross C compiler for the target triple, mirroring the merged sibling carpets:
# standard cross-gcc name on PATH, then the conventional /opt/<triple>-cross prefix, then `zig cc`,
# then musl-gcc for a native x86_64 build. No qemu-user, no target-gcc-under-emulation.
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
        exit 1
    fi
    echo "prebuild: using CC=${CC[*]} for $arch"
}

ensure_host_tools() {
    command -v python3 >/dev/null 2>&1 || { echo "prebuild: python3 required to generate pal.gif" >&2; exit 1; }
    command -v sha256sum >/dev/null 2>&1 || { echo "prebuild: sha256sum required to verify vendored headers" >&2; exit 1; }
    command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
        || { echo "prebuild: curl or wget required to fetch vendored headers" >&2; exit 1; }
}

ROOTFS_SIZE=4G
grow_rootfs() {
    # inject_overlay writes the assets into the base rootfs image without resizing it, so ensure headroom
    # for the ~6 MiB of real rasters before the overlay is injected downstream.
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    command -v resize2fs >/dev/null 2>&1 || { echo "prebuild: resize2fs required (e2fsprogs)" >&2; exit 1; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    # e2fsck exit codes are a bitmask: 0 = clean, 1 = errors were corrected. 2 requests a reboot (n/a for a
    # loopback image), and 4 (uncorrected errors) / 8 (operational error) / >=16 mean the filesystem is not
    # a trustworthy target. Growing and injecting the overlay into an unchecked/corrupt rootfs would treat a
    # broken image as valid input and hide the real failure, so accept only 0/1 and abort loudly otherwise.
    local fsck_rc=0
    e2fsck -f -y "$base_rootfs" || fsck_rc=$?
    if (( fsck_rc > 1 )); then
        echo "prebuild: e2fsck reported unrepaired/operational errors (exit $fsck_rc) on $base_rootfs; refusing to resize/inject a corrupt rootfs" >&2
        exit 2
    fi
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for the image assets"
}

# The four vendored single-header libs, pinned to the upstream commit whose bytes match the calibrated
# goldens. SHA-256 is verified after fetch; a mismatch means the decode/rasterize behaviour would differ
# from what the goldens were computed against, so it is a hard failure.
#   stb      nothings/stb      @ 2c980bb59875b0d32144a71867fbdebb2f77cd20 (stb_image v2.30, stb_image_write v1.16)
#   nanosvg  memononen/nanosvg @ 239e102ec2c691f2902e20ace2ed36ee4a35cfe6
STB_COMMIT=2c980bb59875b0d32144a71867fbdebb2f77cd20
NANOSVG_COMMIT=239e102ec2c691f2902e20ace2ed36ee4a35cfe6
declare -A HEADER_URL=(
    [stb_image.h]="https://raw.githubusercontent.com/nothings/stb/${STB_COMMIT}/stb_image.h"
    [stb_image_write.h]="https://raw.githubusercontent.com/nothings/stb/${STB_COMMIT}/stb_image_write.h"
    [nanosvg.h]="https://raw.githubusercontent.com/memononen/nanosvg/${NANOSVG_COMMIT}/src/nanosvg.h"
    [nanosvgrast.h]="https://raw.githubusercontent.com/memononen/nanosvg/${NANOSVG_COMMIT}/src/nanosvgrast.h"
)
declare -A HEADER_SHA=(
    [stb_image.h]=594c2fe35d49488b4382dbfaec8f98366defca819d916ac95becf3e75f4200b3
    [stb_image_write.h]=cbd5f0ad7a9cf4468affb36354a1d2338034f2c12473cf1a8e32053cb6914a05
    [nanosvg.h]=e34fd5d084be106cea972d19ce5d27fd96d17ba89f8d06bdceee058420c8b2b0
    [nanosvgrast.h]=79a9c5f4db19debf9f3a648a1589e96d92854f245a5cb4f3d823f263785234d8
)

fetch_verify() {
    local dst="$1" url="$2" want="$3" got
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$dst" "$url" || { echo "prebuild: fetch failed: $url" >&2; exit 7; }
    else
        wget -q -O "$dst" "$url" || { echo "prebuild: fetch failed: $url" >&2; exit 7; }
    fi
    got=$(sha256sum "$dst" | cut -d' ' -f1)
    [[ "$got" == "$want" ]] || {
        echo "prebuild: SHA-256 mismatch for $(basename "$dst")" >&2
        echo "prebuild:   want $want" >&2
        echo "prebuild:   got  $got  from $url" >&2
        exit 7
    }
}

# Provision the four gitignored single-header libs into $CAR/third_party (where the cells #include them
# via -I$CAR). If a byte-identical copy is already present (matching SHA), keep it; otherwise fetch and
# verify from the pinned upstream commit.
provision_headers() {
    local tp="$CAR/third_party"; mkdir -p "$tp"
    local h got
    for h in stb_image.h stb_image_write.h nanosvg.h nanosvgrast.h; do
        if [[ -f "$tp/$h" ]]; then
            got=$(sha256sum "$tp/$h" | cut -d' ' -f1)
            if [[ "$got" == "${HEADER_SHA[$h]}" ]]; then
                echo "prebuild: $h present and verified"
                continue
            fi
            echo "prebuild: $h present but SHA mismatch, re-fetching pinned copy" >&2
        fi
        echo "prebuild: fetching $h from pinned upstream"
        fetch_verify "$tp/$h" "${HEADER_URL[$h]}" "${HEADER_SHA[$h]}"
        echo "prebuild: $h fetched and verified"
    done
}

# Each cell is a standalone C program including image_common.h and the pinned single-header libs. A
# compile failure is a genuine breakage. Match the sibling carpets: a plain non-PIE static binary, so
# the target needs no shared image lib and no dynamic loader. -no-pie avoids the riscv64 musl static-PIE
# read-only-reloc link failure and is harmless on the other arches.
compile_cells() {
    local bin="$1" cell
    local cflags=(-O2 -std=c11 -static -no-pie -I"$CAR")
    for cell in image_raster image_formats image_svg image_realassets; do
        echo "prebuild: cross-compile $cell for $arch (bundles stb/nanosvg; self-written SHA-256 + goldens)"
        "${CC[@]}" "${cflags[@]}" "$CAR/$cell.c" -o "$bin/$cell" -lm
        [[ -x "$bin/$cell" ]] || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
    done
}

# Locate render-assets/images (host). Walk up from the app dir if IMAGE_ASSET_SRC is unset.
find_image_src() {
    local src="${IMAGE_ASSET_SRC:-}"
    # Preferred source: the per-app `assets` git submodule (same images/ layout as render-assets).
    # On a fresh CI checkout the gitlink dir exists but is empty until inited, and the rasters arrive as
    # LFS pointers - init + sparse-pull the images/models subdirs so the marker materializes with real bytes.
    if [[ -z "$src" && -d "$app_dir/assets" ]]; then
        if [[ ! -e "$app_dir/assets/images/fmt_ref.png" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        if command -v git >/dev/null 2>&1 && git -C "$app_dir/assets" lfs env >/dev/null 2>&1; then
            git -C "$app_dir/assets" lfs pull --include="images/*,models/*,golden/*" >/dev/null 2>&1 || true
        fi
        [[ -f "$app_dir/assets/images/fmt_ref.png" ]] && src="$app_dir/assets/images"
    fi
    if [[ -z "$src" ]]; then
        local d="$app_dir"
        for _ in 1 2 3 4 5 6; do
            d="$(dirname "$d")"
            if [[ -f "$d/render-assets/images/fmt_ref.png" ]]; then src="$d/render-assets/images"; break; fi
        done
    fi
    echo "$src"
}
find_models_src() {
    # Preferred source: the per-app `assets` submodule (benchy.svg is a plain blob, inited/pulled above).
    if [[ -f "$app_dir/assets/models/benchy.svg" ]]; then echo "$app_dir/assets/models"; return; fi
    local d="$app_dir"
    for _ in 1 2 3 4 5 6; do
        d="$(dirname "$d")"
        if [[ -f "$d/render-assets/models/benchy.svg" ]]; then echo "$d/render-assets/models"; return; fi
    done
}

# Stage the format zoo + real rasters + benchy.svg under /opt/cpu-image-test/assets so the asset-gated
# cells assert against them on-target, and write pal.gif deterministically for the GIF leg. The images are
# a git submodule; on-target the assets are always present, so a required file missing here is a staging
# failure - prebuild hard-fails on zero staged assets so a broken submodule cannot ship a vacuous carpet.
REQUIRED_ASSETS=(fmt_ref.png fmt.bmp fmt.tga fmt.ppm fmt.pgm fmt.jpg
                 honkai3_base.png honkai3_wall_home.png)
stage_assets() {
    local bin="$1" src models f
    src="$(find_image_src)"
    mkdir -p "$bin/assets"
    if [[ -n "$src" && -d "$src" ]]; then
        echo "prebuild: staging images from $src -> /opt/cpu-image-test/assets"
        for f in "${REQUIRED_ASSETS[@]}"; do
            cp -a "$src/$f" "$bin/assets/" 2>/dev/null || echo "prebuild: asset $f absent from source" >&2
        done
    else
        echo "prebuild: render-assets/images not found (set IMAGE_ASSET_SRC)" >&2
    fi
    models="$(find_models_src || true)"
    if [[ -n "${models:-}" && -f "$models/benchy.svg" ]]; then
        cp -a "$models/benchy.svg" "$bin/assets/" && echo "prebuild: staged benchy.svg"
    else
        echo "prebuild: benchy.svg absent from source" >&2
    fi

    # pal.gif: the same 4-colour 64x48 palette pattern the GIF leg regenerates, written as a lossless
    # palette GIF (each pixel index == the pattern's quadrant id). No host codec - so the leg always gates.
    python3 - "$bin/assets/pal.gif" <<'PY' || { echo "prebuild: pal.gif generation failed" >&2; exit 6; }
import sys
w, h = 64, 48
lut = [(20,20,20),(230,30,30),(30,230,30),(30,30,230)]
idx = []
for y in range(h):
    for x in range(w):
        idx.append(((x//16)&1) | (((y//16)&1)<<1))

# Emit each pixel as its own literal LZW code and never let the dictionary grow past the code width:
# re-issue Clear before the decoder would widen. The code width stays fixed at mincode+1, so the stream
# is a trivially-correct GIF LZW - no early-change ambiguity - and decodes byte-exact to the indices.
def lzw_literal(indices, mincode):
    clear = 1 << mincode
    end = clear + 1
    codesize = mincode + 1
    out = bytearray()
    bitbuf = 0
    bitcnt = 0
    def emit(code):
        nonlocal bitbuf, bitcnt
        bitbuf |= code << bitcnt
        bitcnt += codesize
        while bitcnt >= 8:
            out.append(bitbuf & 0xff)
            bitbuf >>= 8
            bitcnt -= 8
    limit = (1 << codesize) - end - 1   # decoder table slots before it would widen the code
    emit(clear)
    since = 0
    for s in indices:
        if since >= limit:
            emit(clear)
            since = 0
        emit(s)
        since += 1
    emit(end)
    if bitcnt > 0:
        out.append(bitbuf & 0xff)
    return out

mincode = 2
comp = lzw_literal(idx, mincode)
gif = bytearray(b"GIF89a")
gif += bytes([w & 0xff, w >> 8, h & 0xff, h >> 8])
# packed: global color table (bit7), color resolution, size of GCT = 2^(1+1) = 4 entries (low 3 bits = 1)
gif += bytes([0xB1, 0, 0])
for c in lut:
    gif += bytes(c)
# image descriptor: position (0,0), size w x h, no local color table
gif += b"\x2C"
gif += bytes([0, 0, 0, 0, w & 0xff, w >> 8, h & 0xff, h >> 8, 0])
gif += bytes([mincode])
i = 0
while i < len(comp):
    block = comp[i:i+255]
    gif += bytes([len(block)]) + block
    i += 255
gif += b"\x00"  # block terminator
gif += b"\x3B"  # trailer
open(sys.argv[1], "wb").write(gif)
PY
    echo "prebuild: staged pal.gif (deterministic 4-colour palette GIF)"

    local staged; staged=$(ls "$bin/assets" 2>/dev/null | wc -l)
    local missing=()
    for f in "${REQUIRED_ASSETS[@]}"; do [[ -f "$bin/assets/$f" ]] || missing+=("$f"); done
    [[ -f "$bin/assets/pal.gif" ]] || missing+=(pal.gif)
    [[ -f "$bin/assets/benchy.svg" ]] || missing+=(benchy.svg)
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "prebuild: required assets did not stage (submodule/LFS failure): ${missing[*]}" >&2
        exit 5
    fi

    # A Git LFS pointer is a small text file that begins with "version https://git-lfs..."; it passes a bare
    # -f existence check but is NOT the real image. Without git-lfs the submodule stages pointers, and the
    # cells would then decode a 130-byte text blob and fail undiagnosably. Reject any staged asset that is a
    # pointer (or otherwise fails its format signature) so a missing LFS object is a loud staging failure,
    # never a vacuous pass.
    local bad=()
    for f in "${REQUIRED_ASSETS[@]}" benchy.svg pal.gif; do
        local p="$bin/assets/$f"
        if IFS= read -r first < "$p" 2>/dev/null && [[ "$first" == version\ https://git-lfs* ]]; then
            bad+=("$f (LFS pointer, real object not pulled)")
            continue
        fi
        local sig; sig=$(od -An -tx1 -N4 "$p" 2>/dev/null | tr -d ' \n')
        case "$f" in
            *.png)  [[ "$sig" == 89504e47* ]]        || bad+=("$f (not a PNG)") ;;
            *.jpg)  [[ "$sig" == ffd8ff* ]]          || bad+=("$f (not a JPEG)") ;;
            *.gif)  [[ "$sig" == 47494638* ]]        || bad+=("$f (not a GIF)") ;;
            *.bmp)  [[ "$sig" == 424d* ]]            || bad+=("$f (not a BMP)") ;;
            *.svg)  grep -qi '<svg' "$p"             || bad+=("$f (not an SVG)") ;;
        esac
    done
    if [[ ${#bad[@]} -gt 0 ]]; then
        echo "prebuild: staged assets failed integrity check (install git-lfs and re-pull the submodule): ${bad[*]}" >&2
        exit 5
    fi
    echo "prebuild: staged $staged asset files (all required present and content-verified)"
}

populate_overlay() {
    local bin="$1"
    : > "$bin/expected_cells"
    for c in image_raster image_formats image_svg image_realassets; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/opt"
    cp -a "$bin" "$overlay_dir/opt/"
    mkdir -p "$overlay_dir/usr/bin"
    ln -sf /opt/cpu-image-test/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/opt/cpu-image-test/assets" 2>/dev/null | cut -f1) assets)"
}

ensure_host_tools
resolve_cc
grow_rootfs
provision_headers

build_root="$(mktemp -d)"
trap 'rm -rf "$build_root"' EXIT
bin="$build_root/cpu-image-test"; mkdir -p "$bin"
compile_cells "$bin"
stage_assets "$bin"
cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
populate_overlay "$bin"
echo "prebuild: cpu-image-test overlay ready for $arch"
