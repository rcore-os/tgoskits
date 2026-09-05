#!/usr/bin/env bash
# prebuild.sh - provision the cpu-gui-qt-test carpet ("pyte for GUI widgets") into the per-arch Alpine rootfs.
#
# The carpet drives real Qt Widgets / QPainter rendering on the CPU RASTER paint engine with the offscreen
# QPA platform plugin (QT_QPA_PLATFORM=offscreen) - NO GPU, NO display server - and asserts CLOSED-FORM
# goldens: exact per-pixel colors from known fillRect/drawLine/drawEllipse geometry, Porter-Duff "over" alpha
# compositing computed by hand, exact layout geometry() from the QVBoxLayout/QHBoxLayout/QGridLayout math, and
# post-event widget state from injected QTest events (mouseClick/keyClicks/keyClick). "Widget created" alone
# is NOT a test - every leg checks a value predicted from first principles.
#
# Libraries: Alpine qt6-qtbase (musl) provides QtCore/QtGui/QtWidgets/QtTest + the offscreen/minimal QPA
# platform plugins; qt6-qtbase-dev gives the headers + pkg-config. The cells LINK against Qt (they TEST Qt;
# they do not reimplement a widget toolkit). Runtime therefore needs the shared Qt libs staged into the
# overlay: this prebuild copies the resolved Qt6 + support libraries and the offscreen platform plugin.
#
# Portable model (same as the model/subtitle/font carpets): extract the base Alpine rootfs, apk add the Qt6
# dev + runtime stack for the TARGET arch via qemu-user, cross-compile each cell on the HOST against the
# apk-staged Qt6, stage the Qt runtime + offscreen plugin + a DejaVu font, and write a capability manifest
# that run_all.sh gates on (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor). The synthetic
# render/layout/interact legs always run; the real-asset font leg honest-skips if no font is staged.
#
# Why the compiler runs on the HOST and not under qemu-user: the target Alpine g++ spawns cc1plus via
# posix_spawn, which qemu-user-static cannot exec, so every in-guest C++ compile fails on cc1plus. apk itself
# runs fine under qemu-user (it only forks/reads/writes), so the Qt6 stack is still provisioned in-guest; only
# the compile+link is moved to a native host cross C++ toolchain that targets the staging root as a sysroot.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"
INSTALL_DIR=/opt/cpu-gui-qt-test

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     triple="aarch64-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     triple="riscv64-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      triple="x86_64-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; triple="loongarch64-linux-musl" ;;
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

# Qt6 + toolchain are large; give the rootfs room.
ROOTFS_SIZE=6G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for the Qt6 stack"
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

# Qt6 widgets + test + the dev headers/pkgconfig to compile against, plus a font for the real-asset leg. The
# compiler is a HOST cross toolchain, so no target g++/build-base is provisioned here; qt6-qtbase-dev carries
# the headers + .pc files, qt6-qtbase the runtime libs + offscreen QPA plugin, font-dejavu+fontconfig a
# deterministic font. musl is pinned so the loader/libc matches the staged Qt build.
PKGS=(musl qt6-qtbase-dev qt6-qtbase font-dejavu fontconfig)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add Qt6 GUI carpet stack (${PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${PKGS[@]}"
    # The cells compile on the HOST, so validate the staged headers/pkgconfig/plugin, not a target g++.
    [[ -d "$staging_root/usr/include/qt6/QtWidgets" ]] \
        || { echo "prebuild: qt6-qtbase-dev headers missing for $arch" >&2; exit 3; }
    [[ -f "$staging_root/usr/lib/pkgconfig/Qt6Widgets.pc" ]] \
        || { echo "prebuild: Qt6 pkgconfig (.pc) missing for $arch" >&2; exit 3; }
    [[ -f "$staging_root/usr/lib/qt6/plugins/platforms/libqoffscreen.so" ]] \
        || { echo "prebuild: offscreen QPA plugin missing for $arch" >&2; exit 3; }
}

# HOST pkgconf resolves the Qt6 -I/-L/-l flags against the staging root: PKG_CONFIG_SYSROOT_DIR makes it
# rewrite the .pc prefix (/usr) to $staging_root, PKG_CONFIG_LIBDIR points it at only the staged .pc files so
# no host Qt leaks in. The emitted -I/-L are absolute paths into the staging root.
host_pkgconf() { command -v pkgconf >/dev/null 2>&1 && echo pkgconf || echo pkg-config; }
PKGCFG() { PKG_CONFIG_SYSROOT_DIR="$staging_root" PKG_CONFIG_LIBDIR="$staging_root/usr/lib/pkgconfig" \
           "$(host_pkgconf)" "$@"; }

# Resolve a HOST cross C++ compiler for $triple. The Alpine Qt6 .so use SHT_RELR (.relr.dyn) relocations, so
# the linker must be RELR-aware: older cross-binutils (GCC 11 era) reject the Qt .so as "incompatible" while
# zig's bundled LLD accepts them. Resolution order (first that produces a valid Qt6-linked ELF wins):
#   1) ${triple}-g++ on PATH               2) /opt/${triple}-cross/bin/${triple}-g++
#   3) zig c++ -target ${triple}           4) host g++ (x86_64 native only)
# Each candidate is proven by actually linking a cell; a candidate whose linker cannot consume the RELR Qt6
# .so is skipped and the next is tried, so the ladder never silently emits an unlinked/partial binary.
#
# CXX_KIND is "gxx" (a g++-style driver invoked with --sysroot) or "zig" (zig c++ -target, no --sysroot: the
# pkgconf -I/-L are already absolute host paths, and zig would double-prepend a sysroot). rpath-link is not
# passed to zig (its LLD ignores it); LLD follows transitive Qt .so deps via the -L path directly.
CXX_CMD=() CXX_KIND=""
resolve_cxx() {
    local zig; zig="$(command -v zig || true)"
    if command -v "${triple}-g++" >/dev/null 2>&1; then
        CXX_CMD=("${triple}-g++"); CXX_KIND="gxx"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then
        CXX_CMD=("/opt/${triple}-cross/bin/${triple}-g++"); CXX_KIND="gxx"
    elif [[ -n "$zig" ]]; then
        CXX_CMD=("$zig" c++ -target "$triple"); CXX_KIND="zig"
    elif [[ "$arch" == "x86_64" ]] && command -v g++ >/dev/null 2>&1; then
        CXX_CMD=(g++); CXX_KIND="gxx"
    else
        echo "prebuild: no host cross C++ toolchain for $triple (tried ${triple}-g++, /opt/${triple}-cross, zig, g++)" >&2
        return 1
    fi
    return 0
}

# Compile one cell on the HOST with the currently-selected toolchain. Returns non-zero on any compile/link
# failure so the caller can fall through to the next candidate.
compile_one() {
    local cell="$1" out="$2" cflags="$3" libs="$4"
    if [[ "$CXX_KIND" == "gxx" ]]; then
        # shellcheck disable=SC2086
        "${CXX_CMD[@]}" --sysroot="$staging_root" -O2 -std=c++17 -fPIC $cflags -I"$CAR" \
            "$CAR/$cell.cpp" -o "$out" $libs -Wl,-rpath-link,"$staging_root/usr/lib"
    else
        # shellcheck disable=SC2086
        "${CXX_CMD[@]}" -O2 -std=c++17 -fPIC $cflags -I"$CAR" "$CAR/$cell.cpp" -o "$out" $libs
    fi
}

# Each cell is a standalone C++ program including gui_common.h + linking Qt6 Widgets/Test. A compile/link
# failure is a genuine breakage. Dynamic link against Qt (the runtime libs are staged into the overlay).
compile_cells() {
    local bin="$1" cell cflags libs
    cflags="$(PKGCFG --cflags Qt6Widgets Qt6Test)"
    libs="$(PKGCFG --libs Qt6Widgets Qt6Test)"
    resolve_cxx || exit 4

    # Probe the resolved toolchain on the first cell; if its linker cannot consume the RELR Qt6 .so, fall to
    # zig (the RELR-aware fallback) before committing to the full set.
    if ! compile_one gui_render "$bin/gui_render" "$cflags" "$libs" 2>"$bin/.cxx_probe.log"; then
        local zig; zig="$(command -v zig || true)"
        if [[ "$CXX_KIND" != "zig" && -n "$zig" ]]; then
            echo "prebuild: ${CXX_CMD[0]} could not link Qt6 for $arch (see below); retrying with zig c++" >&2
            sed 's/^/  /' "$bin/.cxx_probe.log" >&2 || true
            CXX_CMD=("$zig" c++ -target "$triple"); CXX_KIND="zig"
            compile_one gui_render "$bin/gui_render" "$cflags" "$libs" \
                || { echo "prebuild: zig c++ also failed to link Qt6 for $arch" >&2; exit 4; }
        else
            echo "prebuild: cross C++ toolchain failed to link Qt6 for $arch:" >&2
            sed 's/^/  /' "$bin/.cxx_probe.log" >&2 || true; exit 4
        fi
    fi
    rm -f "$bin/.cxx_probe.log"
    [[ -x "$bin/gui_render" ]] || { echo "prebuild: gui_render failed to compile" >&2; exit 4; }
    echo "prebuild: C++ toolchain for $arch = ${CXX_CMD[*]} (--sysroot=$staging_root)"

    for cell in gui_layout gui_interact gui_realassets; do
        echo "prebuild: cross-compile $cell for $arch (links Qt6 Widgets/Test - tests Qt, does not reimplement it)"
        compile_one "$cell" "$bin/$cell" "$cflags" "$libs" \
            || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
        [[ -x "$bin/$cell" ]] || { echo "prebuild: $cell failed to compile" >&2; exit 4; }
    done
}

# Stage the resolved Qt6 runtime + support libraries and the offscreen platform plugin into the overlay, and
# a DejaVu font for the real-asset leg. The cells are dynamically linked against Qt, so target needs these.
stage_runtime() {
    local bin="$1"
    mkdir -p "$overlay_dir/usr/lib/qt6/plugins/platforms" "$bin/assets"
    # copy the whole Qt6 lib set + its transitive support libs that apk pulled in
    echo "prebuild: staging Qt6 runtime libraries into overlay"
    (cd "$staging_root" && find usr/lib -maxdepth 1 \( -name 'libQt6*.so*' \) -print) | while read -r rel; do
        mkdir -p "$overlay_dir/$(dirname "$rel")"; cp -a "$staging_root/$rel" "$overlay_dir/$rel"
    done
    # transitive: copy every shared lib apk installed under usr/lib (Qt pulls harfbuzz, freetype, png, etc.)
    (cd "$staging_root" && find usr/lib -maxdepth 1 -name '*.so*' -print) | while read -r rel; do
        [[ -e "$overlay_dir/$rel" ]] && continue
        cp -a "$staging_root/$rel" "$overlay_dir/$rel" 2>/dev/null || true
    done
    # the offscreen + minimal QPA plugins
    for plug in libqoffscreen.so libqminimal.so; do
        [[ -f "$staging_root/usr/lib/qt6/plugins/platforms/$plug" ]] \
            && cp -a "$staging_root/usr/lib/qt6/plugins/platforms/$plug" "$overlay_dir/usr/lib/qt6/plugins/platforms/"
    done
    # a font for the real-asset leg (staged next to the binaries); honest-skip if none was installed
    local ttf; ttf="$(find "$staging_root/usr/share/fonts" -name '*.ttf' 2>/dev/null | head -1 || true)"
    if [[ -n "$ttf" ]]; then
        cp -a "$ttf" "$bin/assets/" && echo "prebuild: staged font $(basename "$ttf") for real-asset leg"
    else
        echo "prebuild: no .ttf under staging fonts - real-asset leg honest-skips"
    fi
}

compile_carpets() {
    local bin="$staging_root$INSTALL_DIR"; mkdir -p "$bin"
    compile_cells "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    stage_runtime "$bin"
}

populate_overlay() {
    local bin="$staging_root$INSTALL_DIR"
    : > "$bin/expected_cells"
    for c in gui_render gui_layout gui_interact gui_realassets; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
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
compile_carpets
populate_overlay
echo "prebuild: cpu-gui-qt-test overlay ready for $arch"
