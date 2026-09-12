#!/usr/bin/env bash
# prebuild.sh - provision the cpu-tcltk-gui-test carpet ("pyte for GUI widgets") into the per-arch Alpine
# rootfs.
#
# The carpet drives real Tcl/Tk widgets/canvas/photo rendering against a headless X server (Xvfb) and asserts
# CLOSED-FORM goldens: exact photo-image pixels from known put/copy geometry, exact canvas item geometry
# (coords/bbox) from Tk's own canvas layout engine, exact pack/grid/place child geometry from the
# geometry-manager math, exact font measure/metrics, and post-event widget state from injected `event
# generate` mouse/key events. "Widget created" alone is NOT a test - every leg checks a value predicted from
# first principles.
#
# Unlike the Qt carpet (compiled C++ linking Qt6), the Tk cells are Tcl SCRIPTS interpreted by `wish` - so
# there is nothing to cross-compile. Provisioning is: apk add the Alpine tcl/tk runtime + xvfb (headless X
# server) + a DejaVu font for the target arch, stage the .tcl cells + a font asset into the overlay, and
# write a capability manifest that run_all.sh gates on (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor).
# The synthetic render/layout/interact legs always run; the real-asset font leg honest-skips if no resolvable
# font family is present.
#
# The carpet TESTS Tcl/Tk - it does not reimplement a widget toolkit. The only self-written code is the
# three-gate marker + closed-form helpers (gui_common.tcl) and the four cells.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"
INSTALL_DIR=/opt/cpu-tcltk-gui-test

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
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        command -v apt-get >/dev/null 2>&1 && apt-get update && apt-get install -y --no-install-recommends "${missing[@]}" \
            || { echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1; }
    fi
}

# Xorg (Xvfb) + tcl/tk pull a fair amount; give the rootfs room.
ROOTFS_SIZE=4G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for the tcl/tk + Xvfb stack"
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

# tcl/tk runtime + a headless X server (Xvfb) so Tk has a display, plus a DejaVu font for the real-asset leg
# and fontconfig so Tk resolves it. font-dejavu registers "DejaVu Sans Mono" - a fixed-pitch family whose
# closed-form measure the realassets leg asserts on.
PKGS=(tcl tk xvfb font-dejavu fontconfig)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add tcl/tk GUI carpet stack (${PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${PKGS[@]}"
    [[ -x "$staging_root/usr/bin/wish" ]] \
        || { echo "prebuild: wish (tk) not provisioned for $arch" >&2; exit 3; }
    [[ -x "$staging_root/usr/bin/Xvfb" ]] \
        || { echo "prebuild: Xvfb not provisioned for $arch (display backend package missing)" >&2; exit 3; }
}

# Stage the .tcl cells + the runner into the install dir, and a real font into assets/ for the real-asset leg.
stage_carpet() {
    local bin="$1"; mkdir -p "$bin/assets"
    for cell in gui_common gui_render gui_layout gui_interact gui_realassets; do
        cp "$CAR/$cell.tcl" "$bin/$cell.tcl"
    done
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    # a font for the real-asset leg (font-dejavu also registers the family with fontconfig for Tk to resolve)
    local ttf; ttf="$(find "$staging_root/usr/share/fonts" -iname 'DejaVuSansMono*.ttf' 2>/dev/null | head -1 || true)"
    [[ -z "$ttf" ]] && ttf="$(find "$staging_root/usr/share/fonts" -name '*.ttf' 2>/dev/null | head -1 || true)"
    if [[ -n "$ttf" ]]; then
        cp -a "$ttf" "$bin/assets/" && echo "prebuild: staged font $(basename "$ttf") for real-asset leg"
    else
        echo "prebuild: no .ttf under staging fonts - real-asset leg honest-skips"
    fi
}

populate_overlay() {
    local bin="$staging_root$INSTALL_DIR"
    : > "$bin/expected_cells"
    for c in gui_render gui_layout gui_interact gui_realassets; do
        [[ -f "$bin/$c.tcl" ]] && echo "$c" >> "$bin/expected_cells"; done
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
stage_carpet "$staging_root$INSTALL_DIR"
populate_overlay
echo "prebuild: cpu-tcltk-gui-test overlay ready for $arch"
