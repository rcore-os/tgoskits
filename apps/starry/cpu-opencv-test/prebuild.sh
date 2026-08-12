#!/usr/bin/env bash
# prebuild.sh - provision the cpu-opencv-test carpet into the per-arch Alpine rootfs.
#
# The carpet drives real OpenCV on KNOWN, fixed inputs and asserts CLOSED-FORM / numpy goldens computed by
# hand (BT.601 luma, Porter-Duff, the normalized Gaussian kernel, a Sobel gradient's constant derivative,
# bilinear interpolation, an analytic drawn shape, a known step-edge column, a byte-exact PNG/BMP round-trip,
# a lossless FFV1 video round-trip). "cv2 imported" is NOT a test - every leg checks a predicted value.
#
# Two mature, apk-available OpenCV bindings, both TESTED (never reimplemented):
#   C++    - Alpine `opencv` + `opencv-dev` (musl); each cell links libopencv_* via pkg-config `opencv4`.
#   Python - Alpine `py3-opencv` (musl) `import cv2` + `py3-numpy`.
#
# Portable model: extract the base Alpine rootfs, apk add the OpenCV C++/dev + Python stack for the TARGET
# arch via qemu-user (apk runs fine under qemu-user), then cross-compile each C++ cell on the HOST against
# the apk-staged OpenCV, stage the OpenCV runtime + the CPython + cv2 + numpy closure into the overlay, and
# write an expected_cells manifest that run_all.sh gates on (fail==0 && total==EXPECTED==pass, EXPECTED>=1).
#
# Why the C++ cells are compiled on the HOST, not by the target g++ under qemu-user: Alpine's g++ spawns
# cc1plus via posix_spawn, which qemu-user-static cannot exec - so running the staged g++ under qemu always
# fails to compile. Instead a HOST cross C++ compiler builds each cell with --sysroot / include+lib flags
# resolved against the staging root. Two host-toolchain hazards, both resolved below:
#   1. Alpine's OpenCV .so carry a `.relr.dyn` (SHT_RELR) section that older binutils ld (as shipped by the
#      musl-cross ${triple}-g++) rejects as "incompatible". zig's bundled LLD reads .relr.dyn correctly.
#   2. Alpine OpenCV is built against libstdc++ (GNU `std::__cxx11` ABI), but zig c++ defaults to its own
#      libc++ (`std::__1`) - the mangled names would not match. So the cells are compiled with the STAGED
#      GCC libstdc++ headers (`-nostdinc++ -isystem .../c++/<ver>`) and linked against the staged
#      libstdc++.so.6, giving the exact GNU ABI OpenCV expects.
# The synthetic closed-form legs always run; the opencv_io real-asset leg honest-skips if no image is staged.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"
INSTALL_DIR=/opt/cpu-opencv-test
CELLS="opencv_mat opencv_color opencv_filter opencv_geometry opencv_morph opencv_draw opencv_feature opencv_io"

# qemu_runner: runs the target apk (and only apk - never g++) during provisioning.
# triple:      the musl target triple for the HOST cross C++ compiler that builds the cells.
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
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    # pkgconf runs HOST-side now (resolving opencv4 flags against the staging root).
    command -v pkgconf >/dev/null 2>&1 || command -v pkg-config >/dev/null 2>&1 || missing+=(pkgconf)
    if [[ ${#missing[@]} -gt 0 ]]; then
        command -v apt-get >/dev/null 2>&1 && apt-get update && apt-get install -y --no-install-recommends "${missing[@]}" \
            || { echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1; }
    fi
    # A HOST C++ cross toolchain for the cells: ${triple}-g++ (PATH or /opt) or zig c++. resolve_cxx probes
    # which one actually links Alpine's .relr.dyn OpenCV; here just fail early if neither exists at all.
    command -v "${triple}-g++" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]] \
        || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host C++ cross toolchain (need ${triple}-g++ or zig) for $arch" >&2; exit 1; }
}

# OpenCV + a full CPython + the C++ toolchain are large; give the rootfs room.
ROOTFS_SIZE=6G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for the OpenCV stack"
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

# C++ opencv + dev headers to compile against; python3 + py3-opencv (cv2) + py3-numpy for the Python cells.
# build-base is staged so that the target libstdc++ headers (usr/include/c++/<ver>) and libstdc++.so.6 are
# present - the HOST cross compiler builds the cells against exactly those, matching OpenCV's GNU C++ ABI.
# pkgconf is used HOST-side (a host pkgconf run against the staging root's opencv4.pc). pulseaudio-libs is an
# OPTIONAL runtime dep of opencv's highgui (libpulse -> libpulsecommon); the tested cells are all offscreen/
# computational and do not use highgui, so add it best-effort where Alpine packages it (x86_64/aarch64) and
# skip it where absent (riscv64/loongarch64) instead of hard-failing the whole provision.
PKGS=(musl build-base pkgconf opencv opencv-dev py3-opencv py3-numpy python3)
OPT_PKGS=(pulseaudio-libs)

# Write a resolv.conf into the staging root that is actually reachable from inside qemu-user. Host loopback
# stub resolvers (127.0.0.0/8, e.g. systemd-resolved 127.0.0.53) are dropped; real host nameservers are kept
# first, then reachable public resolvers are appended as a fallback. STARRY_DNS (space/comma-separated IPs)
# overrides the public fallback list.
provision_resolv_conf() {
    local rc="$staging_root/etc/resolv.conf" ns
    mkdir -p "$staging_root/etc"
    : > "$rc"
    if [[ -f /etc/resolv.conf ]]; then
        while read -r kw ns _; do
            [[ "$kw" == nameserver ]] || continue
            [[ "$ns" == 127.* || "$ns" == ::1 ]] && continue
            echo "nameserver $ns" >> "$rc"
        done < /etc/resolv.conf
    fi
    local fallback="${STARRY_DNS:-1.1.1.1 8.8.8.8 9.9.9.9}"
    for ns in ${fallback//,/ }; do
        grep -qx "nameserver $ns" "$rc" 2>/dev/null || echo "nameserver $ns" >> "$rc"
    done
    echo "prebuild: staging resolv.conf -> $(tr '\n' ' ' < "$rc")"
}

apk_provision() {
    normalize_symlinks
    # DNS for apk under qemu-user: the host's /etc/resolv.conf often points at a loopback stub
    # (systemd-resolved / Docker's 127.0.0.53), whose listener does not exist inside the qemu-user staging
    # root, so apk's `Alpine repo -> DNS: transient error`. Carry over only the host's non-loopback
    # nameservers, and always append reachable public resolvers as a fallback so apk can resolve the Alpine
    # CDN regardless of the host stub. STARRY_DNS overrides the fallback list when set.
    provision_resolv_conf
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add OpenCV C++/Python carpet stack (${PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${PKGS[@]}"
    # optional highgui audio backend: packaged for x86_64/aarch64, absent for riscv64/loongarch64 - best-effort
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${OPT_PKGS[@]}" 2>/dev/null \
        || echo "prebuild: optional ${OPT_PKGS[*]} not packaged for $arch - skipped (offscreen cells do not use highgui audio)"
    [[ -f "$staging_root/usr/lib/libstdc++.so.6" ]] \
        || { echo "prebuild: libstdc++.so.6 (build-base) not provisioned for $arch" >&2; exit 3; }
    [[ -n "$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)" ]] \
        || { echo "prebuild: staged libstdc++ headers (usr/include/c++) missing for $arch" >&2; exit 3; }
    [[ -f "$staging_root/usr/lib/pkgconfig/opencv4.pc" ]] \
        || { echo "prebuild: opencv4.pc (opencv-dev) missing for $arch" >&2; exit 3; }
    [[ -x "$staging_root/usr/bin/python3" ]] \
        || { echo "prebuild: python3 not provisioned for $arch" >&2; exit 3; }
}

# PKGCFG - resolve OpenCV include/lib flags on the HOST against the apk-staged opencv4.pc. Sysroot-prefixed
# so the emitted -I/-L point into the staging root. The .pc itself is target-arch-neutral (just paths).
host_pkgconf() { if command -v pkgconf >/dev/null 2>&1; then echo pkgconf; else echo pkg-config; fi; }
PKGCFG() { PKG_CONFIG_SYSROOT_DIR="$staging_root" PKG_CONFIG_LIBDIR="$staging_root/usr/lib/pkgconfig" \
           "$(host_pkgconf)" "$@"; }

# CXX_COMPILE / CXX_LINK - the HOST C++ cross toolchain for $triple, resolved once. Preference order mirrors
# the cpu-concurrency carpet but for C++, with the two OpenCV-specific constraints (.relr.dyn + GNU C++ ABI):
#   1) zig c++ -target <triple>   - LLD reads Alpine's .relr.dyn; combined with the STAGED libstdc++ headers
#                                   and libstdc++.so.6 it produces the exact GNU (`std::__cxx11`) ABI OpenCV
#                                   was built against. This is the working path (verified on all arches).
#   2) ${triple}-g++ on PATH / under /opt/${triple}-cross - a native GNU cross g++. Correct ABI by
#      construction, but only usable if its binutils ld can read .relr.dyn; probed for real at resolve time.
# Resolution stores a mode in $cxx_mode; the compile/link helpers dispatch on it. cxx_gpp holds the g++ path.
cxx_mode=""; cxx_gpp=""; cxx_incflags=(); cxx_lldflags=()
# resolve_lld: locate a standalone ld.lld (PATH, then versioned Debian/Ubuntu names) and symlink it into a
# private -B dir so `${triple}-g++ -fuse-ld=lld` finds it as plain `ld.lld`. Sets cxx_lldflags, returns 0.
resolve_lld() {
    local lld="" c
    if command -v ld.lld >/dev/null 2>&1; then lld="$(command -v ld.lld)"; fi
    if [[ -z "$lld" ]]; then
        for c in /usr/bin/ld.lld /usr/lib/llvm-*/bin/ld.lld; do [[ -x "$c" ]] && { lld="$c"; break; }; done
    fi
    [[ -n "$lld" ]] || return 1
    local d; d="$(mktemp -d)"; ln -sf "$lld" "$d/ld.lld"; cxx_lldflags=(-fuse-ld=lld -B"$d"); return 0
}
resolve_cxx() {
    local gxxver cxxinc cxxinc_tri gpp
    # STAGED libstdc++ headers - shared by both zig and g++ probing (g++ ships its own, zig needs these).
    gxxver="$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)"
    cxxinc="$gxxver"
    cxxinc_tri="$(ls -d "$gxxver"/*-alpine-linux-musl 2>/dev/null | head -1)"

    # candidate GNU cross g++ (PATH, then conventional /opt install prefix)
    if command -v "${triple}-g++" >/dev/null 2>&1; then gpp="${triple}-g++"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/${triple}-cross/bin/${triple}-g++"
    else gpp=""; fi

    # Prefer g++ only if it can actually link an Alpine .relr.dyn .so - probe against libopencv_core.
    if [[ -n "$gpp" ]]; then
        local probe; probe="$(mktemp)"
        printf 'int main(){return 0;}\n' > "$probe.cpp"
        if "$gpp" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -L"$staging_root/usr/lib" -lopencv_core >/dev/null 2>&1; then
            cxx_mode="gpp"; cxx_gpp="$gpp"; rm -f "$probe" "$probe.cpp"
            echo "prebuild: C++ toolchain = $gpp (--sysroot, native GNU ABI, ld reads .relr.dyn)"; return 0
        fi
        # g++'s bundled GNU ld 2.37 has no RELR; retry the same native g++ with LLD (keeps the GNU C++ ABI,
        # so no staged-libstdc++ header juggling) before falling back to zig. Preferred over zig when host
        # LLD is available, which is the common CI case.
        if resolve_lld && "$gpp" "${cxx_lldflags[@]}" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -L"$staging_root/usr/lib" -lopencv_core >/dev/null 2>&1; then
            cxx_mode="gpp"; cxx_gpp="$gpp"; rm -f "$probe" "$probe.cpp"
            echo "prebuild: C++ toolchain = $gpp -fuse-ld=lld (--sysroot, native GNU ABI, LLD reads .relr.dyn)"; return 0
        fi
        cxx_lldflags=()
        rm -f "$probe" "$probe.cpp"
    fi

    # Fall back to zig c++ + staged libstdc++ headers/lib.
    if command -v zig >/dev/null 2>&1; then
        [[ -d "$cxxinc" ]] || { echo "prebuild: no staged libstdc++ headers for zig c++ path" >&2; exit 4; }
        cxx_mode="zig"; cxx_incflags=(-nostdinc++ -isystem "$cxxinc")
        [[ -n "$cxxinc_tri" ]] && cxx_incflags+=(-isystem "$cxxinc_tri")
        # staged musl libc headers reachable for libstdc++'s include_next chain
        cxx_incflags+=(-idirafter "$staging_root/usr/include")
        echo "prebuild: C++ toolchain = zig c++ -target $triple (staged libstdc++ headers, LLD reads .relr.dyn)"
        return 0
    fi

    echo "prebuild: no host C++ cross toolchain for $triple (tried ${triple}-g++, /opt/${triple}-cross, zig c++)" >&2
    exit 4
}

# Compile one C++ cell to an object. zig is cache-sensitive on the combined compile+link step (it can reuse
# a stale object), so cells are always compiled to a .o first, then linked - deterministic and correct.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -O2 -std=c++17 -fPIC "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 -fPIC "$@" -c "$src" -o "$obj" ;;
    esac
}

# Link one object into a target ELF against the staged OpenCV. For zig the staged libstdc++.so.6 is passed
# positionally so its GNU-ABI symbols resolve; g++ pulls its own libstdc++ implicitly.
cxx_link() {
    local obj="$1" out="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gpp" "${cxx_lldflags[@]}" --sysroot="$staging_root" "$obj" -o "$out" "$@" ;;
        zig) zig c++ -target "$triple" "$obj" -o "$out" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

# Each C++ cell is a standalone program including cv_common.h + linking libopencv_* via pkg-config opencv4.
# A compile failure is a genuine breakage. Dynamic link against OpenCV (the runtime .so are staged).
compile_cells() {
    local bin="$1" cell cflags libs obj
    resolve_cxx
    cflags="$(PKGCFG --cflags opencv4)"
    # The cells use core/imgproc/imgcodecs/features2d/videoio only (never highgui). Drop -lopencv_highgui so
    # its GUI-backend NEEDED (Alpine libQt6Core, which references a newer-musl renameat2 the cross toolchain's
    # libc lacks) is not pulled into the link - a link-only trim, the runtime .so closure is unchanged.
    libs="$(PKGCFG --libs opencv4 | sed 's/-lopencv_highgui[^ ]*//g')"
    mkdir -p "$bin/cpp"
    for cell in $CELLS; do
        echo "prebuild: host cross-compile cpp/$cell for $arch (links libopencv_* - tests OpenCV, does not reimplement it)"
        obj="$bin/cpp/$cell.o"
        # shellcheck disable=SC2086
        cxx_object "$CAR/cpp/$cell.cpp" "$obj" $cflags -I"$CAR/cpp"
        # shellcheck disable=SC2086
        cxx_link "$obj" "$bin/cpp/$cell" $libs
        rm -f "$obj"
        [[ -x "$bin/cpp/$cell" ]] || { echo "prebuild: cpp/$cell failed to compile/link" >&2; exit 4; }
    done
}

# The Python cells import cv2 (py3-opencv) + numpy on target. Stage them only where cv2 actually imports:
# on riscv64 Alpine's py3-opencv pulls libQt6Core.so.6, which references a newer-musl renameat2 the base
# rootfs musl does not export, so `import cv2` is unresolvable - a documented Alpine ecosystem wall (the
# C++ cells linked without highgui avoid it). Probe once under qemu-user; where cv2 imports (x86_64/aarch64)
# stage + gate the Python cells, where it cannot honestly skip them so the manifest reflects real capability.
stage_python_cells() {
    local bin="$1"
    rm -rf "$bin/py"; mkdir -p "$bin/py"
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -c "import cv2" >/dev/null 2>&1; then
        cp "$CAR/py/"*.py "$bin/py/"
    else
        echo "prebuild: cv2 import fails on $arch (Alpine py3-opencv -> libQt6Core renameat2 wall) - Python cells honestly skipped this arch; the C++ cells still exercise the full OpenCV surface"
    fi
}

# Stage the OpenCV runtime + its transitive support libs and the CPython/cv2/numpy closure into the overlay,
# so the dynamically-linked C++ cells and the `import cv2` Python cells run on target.
stage_runtime() {
    echo "prebuild: staging OpenCV + CPython/cv2/numpy runtime into overlay"
    # every shared lib apk installed under usr/lib (opencv + its ffmpeg/png/jpeg/tiff/webp/tbb closure)
    (cd "$staging_root" && find usr/lib -maxdepth 1 -name '*.so*' -print) | while read -r rel; do
        mkdir -p "$overlay_dir/$(dirname "$rel")"; cp -a "$staging_root/$rel" "$overlay_dir/$rel" 2>/dev/null || true
    done
    # opencv pulls libpulse whose real object lives under usr/lib/pulseaudio
    if [[ -d "$staging_root/usr/lib/pulseaudio" ]]; then
        mkdir -p "$overlay_dir/usr/lib/pulseaudio"
        cp -a "$staging_root/usr/lib/pulseaudio/." "$overlay_dir/usr/lib/pulseaudio/" 2>/dev/null || true
    fi
    # the CPython interpreter + stdlib + site-packages (cv2 + numpy)
    local pyver
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    [[ -n "$pyver" ]] || { echo "prebuild: no python3.x under staging" >&2; exit 5; }
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/bin"
    cp -a "$staging_root/usr/lib/$pyver" "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/bin/python3" "$overlay_dir/usr/bin/" 2>/dev/null || true
    [[ -e "$staging_root/usr/bin/$pyver" ]] && cp -a "$staging_root/usr/bin/$pyver" "$overlay_dir/usr/bin/" || true
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true
}

# Stage the real images the opencv_io leg decodes (flat, next to the cells). opencv_io reads every image
# directly under ASSET_DIR (=$bin/assets), so the submodule's images/ subdir is flattened here. A missing
# submodule never fails the gate (the closed-form legs still run); present images are asserted to decode.
stage_real_assets() {
    local bin="$1" src="${OPENCV_ASSET_SRC:-}"
    # Preferred source: the per-app `assets` git submodule (images/ + golden/ layout). On a fresh CI
    # checkout the gitlink dir exists but is empty until inited, and the rasters arrive as LFS pointers -
    # init + sparse-pull the subdirs this carpet reads so the marker materializes with real bytes.
    if [[ -z "$src" && -d "$app_dir/assets" ]]; then
        if [[ ! -e "$app_dir/assets/images/fmt_ref.png" ]] && command -v git >/dev/null 2>&1; then
            git -C "$app_dir" submodule update --init assets >/dev/null 2>&1 || true
        fi
        if command -v git >/dev/null 2>&1 && git -C "$app_dir/assets" lfs env >/dev/null 2>&1; then
            git -C "$app_dir/assets" lfs pull --include="images/*,video/*,audio/*,golden/*" >/dev/null 2>&1 || true
        fi
        [[ -f "$app_dir/assets/images/fmt_ref.png" ]] && src="$app_dir/assets/images"
    fi
    if [[ -n "$src" && -d "$src" ]]; then
        echo "prebuild: staging real images from $src -> $INSTALL_DIR/assets"
        cp -a "$src"/*.png "$src"/*.jpg "$src"/*.bmp "$src"/*.ppm "$src"/*.tiff "$bin/assets/" 2>/dev/null || true
        echo "prebuild: staged $(find "$bin/assets" -maxdepth 1 -type f 2>/dev/null | wc -l) image files for opencv_io"
    else
        echo "prebuild: no image asset source found - opencv_io real-asset leg will honest-skip on-target"
    fi
}

compile_carpets() {
    local bin="$staging_root$INSTALL_DIR"; mkdir -p "$bin/assets"
    compile_cells "$bin"
    stage_python_cells "$bin"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    stage_real_assets "$bin"
    stage_runtime
}

populate_overlay() {
    local bin="$staging_root$INSTALL_DIR"
    : > "$bin/expected_cells"
    for c in $CELLS; do
        [[ -x "$bin/cpp/$c" ]] && echo "cpp/$c" >> "$bin/expected_cells"
        [[ -f "$bin/py/$c.py" ]] && echo "py/$c"  >> "$bin/expected_cells"
    done
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
echo "prebuild: cpu-opencv-test overlay ready for $arch"
