#!/usr/bin/env bash
# prebuild.sh - provision the software OpenGL ES compute runtime (Mesa llvmpipe + the EGL
# surfaceless platform) and the compiled GLES compute carpet binaries into the per-arch Alpine
# rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-gles (the
# GLES 3.1 client library over llvmpipe), mesa-egl (EGL, including the EGL_MESA_platform_surfaceless
# path used to create a headless context), mesa-dri-gallium (the llvmpipe CPU driver) and the build
# toolchain INTO it via qemu-user-static (apk resolves every package for the TARGET arch on an x86
# build host - no drifting URLs, no cache-miss-exit), then cross-compile the GLES C and C++ carpet
# sources HOST-side with a musl cross toolchain (${triple}-gcc/g++ or zig cc/c++) against the staged
# mesa sysroot - the target gcc cannot run under qemu-user (it spawns cc1/collect2 via posix_spawn,
# which qemu-user cannot exec). The arch-independent EGL/GLES2/GLES3/KHR client headers are vendored
# under programs/headers; the glesv2/egl link flags come from a HOST pkgconf run against the staged
# mesa-dev .pc. Finally copy the shared-library closure and the carpet binaries + runner into the
# overlay. Inputs are the base rootfs and the Alpine edge apk repos only.
#
# All backends are CPU software: llvmpipe runs the GLES 3.1 compute pipeline on the LLVM CPU JIT and
# EGL creates the context on EGL_PLATFORM=surfaceless, so no host GPU or display server is required.
# Alpine edge builds mesa-gles / mesa-egl / mesa-dri-gallium for all four target arches
# (x86_64 / aarch64 / riscv64 / loongarch64), so the C/C++ carpets run on-target on every arch.
#
# The Python cell (gles_py) runs on-target on every arch through PyOpenGL over the same surfaceless-EGL
# llvmpipe context as the C/C++ cells (moderngl is desktop-GL only and Alpine ships no py3-moderngl for
# any arch, so the Python GLES cell uses PyOpenGL's OpenGL.EGL + OpenGL.GLES3). py3-opengl resolves on
# every arch (Alpine community), so it is a required package like mesa-gles/mesa-egl. The Rust cell
# (glow + khronos-egl) likewise runs on-target as a dynamic musl binary.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"

# qemu_runner: apk still resolves the TARGET rootfs under qemu-user (only gcc/cc1 was broken there).
# triple:      the musl target triple for the HOST cross C/C++ compiler that builds the cells (gcc under
#              qemu-user cannot spawn cc1/collect2 via posix_spawn, so all compiling/linking is HOST-side).
# rust_target: the Rust cross target (cargo cross-compiles natively - unchanged).
case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     triple="aarch64-linux-musl";     rust_target="aarch64-unknown-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     triple="riscv64-linux-musl";     rust_target="riscv64gc-unknown-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      triple="x86_64-linux-musl";      rust_target="x86_64-unknown-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; triple="loongarch64-linux-musl"; rust_target="loongarch64-unknown-linux-musl" ;;
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
    # HOST pkgconf resolves the staged glesv2/egl flags against the staging root, and a HOST C/C++ cross
    # toolchain (${triple}-gcc/-g++ or zig) links the Alpine mesa .so - target gcc under qemu-user cannot
    # spawn cc1/collect2 via posix_spawn, so both compiling and linking run HOST-side.
    command -v pkgconf >/dev/null 2>&1 || command -v pkg-config >/dev/null 2>&1 \
        || { echo "prebuild: host pkgconf/pkg-config required" >&2; exit 1; }
    command -v "${triple}-g++" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]] \
        || [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]] || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host C/C++ cross toolchain (need ${triple}-g++ or zig) for $arch" >&2; exit 1; }
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

# The harness injects $STARRY_OVERLAY_DIR into $base_rootfs via debugfs WITHOUT resizing, so the
# per-app image must be grown here first. The overlay carries the full mesa closure plus its LLVM
# runtime (~200 MiB); the stock ~2 GiB image overflows and debugfs silently truncates the backend
# libraries ("Could not allocate block"), which surfaces at runtime as "symbol not found". 4 GiB
# leaves ample headroom. Idempotent: truncate only grows, e2fsck/resize2fs are safe to re-run. The
# image stays sparse on the host.
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
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB (fs resized) for mesa/llvmpipe closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa GLES client + EGL + the llvmpipe DRI driver + LLVM + build toolchain, all musl for the target
# arch. mesa-dev is staged so glesv2.pc / egl.pc exist for the HOST pkgconf run that resolves the cell
# link flags; build-base stages libstdc++.so.6 + its headers, needed by the HOST C++ cross link. Alpine
# builds mesa-gles / mesa-egl / mesa-dev / mesa-dri-gallium for every arch. The EGL/GLES/KHR client
# headers are still vendored under programs/headers (-I"$hdr") for the compile step.
GPU_PKGS=(musl mesa-gles mesa-egl mesa-dev mesa-dri-gallium
          build-base pkgconf
          gmp mpfr4 mpc1 isl26 zlib
          python3 py3-numpy)
# PyOpenGL (the gles_py cell) - pure-python GLES binding over the surfaceless-EGL llvmpipe context,
# from apk (py3-opengl, present in Alpine community on every arch). It is required on-target like the
# mesa packages: py3-opengl resolves for x86_64/aarch64/riscv64/loongarch64, so gles_py is wired on
# every arch (never a vacuous pass).
PY_BINDING_PKGS=(py3-opengl)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add GLES stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -f "$staging_root/usr/lib/libEGL.so.1" || -e "$staging_root/usr/lib/libEGL.so" ]] || { echo "prebuild: mesa-egl (libEGL) not provisioned" >&2; exit 3; }
    { [[ -f "$staging_root/usr/lib/libGLESv2.so.2" || -e "$staging_root/usr/lib/libGLESv2.so" ]]; } || { echo "prebuild: mesa-gles (libGLESv2) not provisioned" >&2; exit 3; }
    # PyOpenGL (gles_py) is required on-target; py3-opengl resolves on every arch (Alpine community).
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${PY_BINDING_PKGS[@]}"; then
        echo "prebuild: PyOpenGL (gles_py) provisioned for $arch"
    else
        echo "prebuild: py3-opengl unavailable for $arch (apk could not resolve; gles_py cell not wired this arch)"
    fi
}

# HOST pkgconf resolving the staged glesv2/egl .pc against the staging root (target gcc under qemu-user
# cannot spawn cc1, so linking runs HOST-side against the staged sysroot).
host_pkgconf() { if command -v pkgconf >/dev/null 2>&1; then echo pkgconf; else echo pkg-config; fi; }
PKGCFG() { PKG_CONFIG_SYSROOT_DIR="$staging_root" PKG_CONFIG_LIBDIR="$staging_root/usr/lib/pkgconfig" \
           "$(host_pkgconf)" "$@"; }
libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

# The HOST C/C++ cross toolchain for $triple, resolved once. The cells link Alpine's mesa libGLESv2 /
# libEGL, which carry a `.relr.dyn` (SHT_RELR) section older musl-cross binutils ld rejects; zig's
# bundled LLD reads it. Preference order (mirrors the merged sibling carpets):
#   1) ${triple}-g++/-gcc (PATH or /opt) - native GNU ABI; used only if its ld can link a mesa .relr.dyn .so.
#   2) zig c++/cc -target <triple> + STAGED libstdc++ headers/lib - LLD reads .relr.dyn, GNU (std::__cxx11) ABI.
cxx_mode=""; cxx_gpp=""; cxx_gcc=""; cxx_incflags=()
resolve_cxx() {
    local gxxver cxxinc cxxinc_tri gpp gcc probelib
    gxxver="$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)"
    cxxinc="$gxxver"
    cxxinc_tri="$(ls -d "$gxxver"/*-alpine-linux-musl 2>/dev/null | head -1)"

    if command -v "${triple}-g++" >/dev/null 2>&1; then gpp="${triple}-g++"; gcc="${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/${triple}-cross/bin/${triple}-g++"; gcc="/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/cross/${triple}-cross/bin/${triple}-g++"; gcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
    else gpp=""; gcc=""; fi

    # Prefer g++ only if its ld can actually link an Alpine .relr.dyn mesa .so - probe against libGLESv2.
    probelib="$(libpath libGLESv2)"
    if [[ -n "$gpp" && -n "$probelib" ]]; then
        local probe; probe="$(mktemp)"
        printf 'int main(){return 0;}\n' > "$probe.cpp"
        if "$gpp" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -L"$staging_root/usr/lib" -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" \
                -lGLESv2 >/dev/null 2>&1; then
            cxx_mode="gpp"; cxx_gpp="$gpp"; cxx_gcc="$gcc"; rm -f "$probe" "$probe.cpp"
            echo "prebuild: C/C++ toolchain = $gpp / $gcc (--sysroot, native GNU ABI, ld reads .relr.dyn)"; return 0
        fi
        rm -f "$probe" "$probe.cpp"
    fi

    if command -v zig >/dev/null 2>&1; then
        [[ -d "$cxxinc" ]] || { echo "prebuild: no staged libstdc++ headers for zig c++ path" >&2; exit 4; }
        cxx_mode="zig"; cxx_incflags=(-nostdinc++ -isystem "$cxxinc")
        [[ -n "$cxxinc_tri" ]] && cxx_incflags+=(-isystem "$cxxinc_tri")
        cxx_incflags+=(-idirafter "$staging_root/usr/include")
        echo "prebuild: C/C++ toolchain = zig cc/c++ -target $triple (staged libstdc++ headers, LLD reads .relr.dyn)"
        return 0
    fi

    echo "prebuild: no host C/C++ cross toolchain for $triple (tried ${triple}-g++, /opt, zig)" >&2
    exit 4
}

# Compile one C cell to a .o (zig reuses a stale object on a combined step, so split compile/link).
cc_object() {
    local src="$1" obj="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gcc" --sysroot="$staging_root" -O2 -std=c11 "$@" -c "$src" -o "$obj" ;;
        zig) zig cc -target "$triple" -idirafter "$staging_root/usr/include" -O2 -std=c11 "$@" -c "$src" -o "$obj" ;;
    esac
}
# Link one C object into a target ELF against the staged mesa. -Wl,-rpath-link resolves the mesa .so closure.
cc_link() {
    local obj="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gcc" --sysroot="$staging_root" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig cc -target "$triple" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
    esac
}

# Compile one C++ cell to a .o first, then link - zig reuses a stale object on the combined step, so split.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
    esac
}
# Link one C++ object into a target ELF against the staged mesa. zig gets the staged libstdc++.so.6
# positionally (GNU-ABI symbols); g++ pulls its own libstdc++ implicitly.
cxx_link() {
    local obj="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig c++ -target "$triple" "$obj" -o "$out" "${rpl[@]}" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

# write_rust_linker: cargo cross-compiles the glow/khronos-egl Rust cell natively, but its link step
# needs a musl cross linker. The target Alpine gcc under qemu-user cannot spawn collect2/ld, so the
# cargo linker points at the HOST cross gcc (${triple}-gcc on PATH -> /opt -> zig cc -> x86_64 musl-gcc).
# This cell dlopens libEGL at runtime, so the link does not need the mesa .so - only a musl C linker.
write_rust_linker() {
    local ccwrap="$1" hostcc=""
    if command -v "${triple}-gcc" >/dev/null 2>&1; then hostcc="${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then hostcc="/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]]; then hostcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then hostcc="musl-gcc"; fi
    if [[ -n "$hostcc" ]]; then
        printf '#!/bin/sh\nexec %s "$@"\n' "$hostcc" > "$ccwrap"
    elif command -v zig >/dev/null 2>&1; then
        printf '#!/bin/sh\nexec zig cc -target %s "$@"\n' "$triple" > "$ccwrap"
    else
        echo "prebuild: no host cross C linker for $triple (Rust cell)" >&2; return 1
    fi
    chmod +x "$ccwrap"
}

# Cross-compile the glow (Rust) GLES cell to a dynamic musl binary. glow loads GL entry points via a
# loader closure and khronos-egl's "dynamic" feature dlopen()s libEGL at runtime, so nothing is
# C-linked at build time - but the binary MUST be dynamic musl: a static musl binary's dlopen is a
# NULL stub, which would give a vacuous no-context green. Built on every arch (mesa-egl/gles are
# present 4-arch); the EGL/GLES sonames it dlopens ride the same /usr/lib closure as the C/C++ cells.
compile_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the glow (Rust) cell" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"
    mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    echo "prebuild: cross-compile glow (Rust) cell for $arch -> $rust_target (dynamic musl; khronos-egl dlopens libEGL at runtime)"
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    local cargohome; cargohome="$(mktemp -d)"
    ( cd "$CAR/gles_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --locked --target "$rust_target" 2>&1 | tail -5 ) || \
    ( cd "$CAR/gles_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --target "$rust_target" 2>&1 | tail -5 )
    rm -rf "$cargohome"
    local rustbin="$CAR/gles_rust/target/$rust_target/release/gles_rust_full_api"
    [[ -x "$rustbin" ]] || { echo "prebuild: glow (Rust) cell failed to cross-compile for $arch" >&2; exit 5; }
    cp "$rustbin" "$bin/gles_rust"
    echo "prebuild: glow (Rust) cell -> /opt/cpu-gles-compute/gles_rust ($(stat -c %s "$rustbin") bytes, dynamic musl)"
}

# gles_py cell: PyOpenGL GLES 3.1 compute over surfaceless EGL. python3 + numpy + PyOpenGL come from
# apk, carried into the overlay by populate_overlay's cp -a of /usr/lib/. Wired where py3-opengl
# resolved (Alpine community, every arch). The wrapper exports the surfaceless-EGL/llvmpipe env just
# like the C EGL cell and the render/gles python cell; the .py selects PYOPENGL_PLATFORM=egl before
# import.
provision_python() {
    local bin="$staging_root/opt/cpu-gles-compute"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 not provisioned - gles_py cell not wired" >&2; return 0; }
    ls -d "$staging_root"/usr/lib/python3*/site-packages/OpenGL >/dev/null 2>&1 \
        || { echo "prebuild: PyOpenGL absent for $arch - gles_py cell not wired"; return 0; }
    cp "$CAR/gles_py/gles_py_full_api.py" "$bin/gles_py.py"
    cat > "$bin/gles_py" <<'PYW'
#!/bin/sh
export PYOPENGL_PLATFORM=egl
export EGL_PLATFORM=surfaceless
export GALLIUM_DRIVER=llvmpipe
export LIBGL_ALWAYS_SOFTWARE=1
export LP_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-gles-compute/gles_py.py "$@"
PYW
    chmod +x "$bin/gles_py"
    echo "prebuild: gles_py -> /opt/cpu-gles-compute/gles_py (python3 + numpy + PyOpenGL, surfaceless EGL)"
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-gles-compute"; mkdir -p "$bin"
    local hdr="$app_dir/programs/headers" cflags libs obj
    [[ -n "$(libpath libEGL)" ]] || { echo "prebuild: libEGL not provisioned" >&2; exit 4; }
    [[ -n "$(libpath libGLESv2)" ]] || { echo "prebuild: libGLESv2 not provisioned" >&2; exit 4; }
    resolve_cxx
    # glesv2/egl link + include flags resolved by a HOST pkgconf run against the staged mesa-dev .pc.
    cflags="$(PKGCFG --cflags glesv2 egl)"
    libs="$(PKGCFG --libs glesv2 egl)"

    echo "prebuild: host cross-compile GLES carpets for $arch (llvmpipe compute, EGL surfaceless)"
    # GLES 3.1 compute over EGL-surfaceless + GLESv2. The vendored EGL/GLES2/GLES3/KHR client headers
    # (-I"$hdr") declare the API; libGLESv2/libEGL are linked HOST-side against the staged sysroot (their
    # .relr.dyn sections need zig's LLD or a probed g++ ld).
    obj="$bin/gles_c.o"
    # shellcheck disable=SC2086
    cc_object "$CAR/gles_c/gles_c_full_api.c" "$obj" -I"$hdr" $cflags
    # shellcheck disable=SC2086
    cc_link "$obj" "$bin/gles_c" $libs -lm
    rm -f "$obj"
    obj="$bin/gles_cpp.o"
    # shellcheck disable=SC2086
    cxx_object "$CAR/gles_cpp/gles_cpp_full_api.cpp" "$obj" -I"$hdr" $cflags
    # shellcheck disable=SC2086
    cxx_link "$obj" "$bin/gles_cpp" $libs -lm
    rm -f "$obj"
    for f in gles_c gles_cpp; do
        [[ -x "$bin/$f" ]] || { echo "prebuild: carpet $f failed to compile" >&2; exit 4; }
    done
    compile_rust "$bin"
    provision_python
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) GLES carpet binary(ies) + run_all.sh"
}

populate_overlay() {
    local bin="$staging_root/opt/cpu-gles-compute"
    # Capability manifest: list exactly the cells provisioned on this arch. Every cell build hard-fails
    # (compile_carpets / compile_rust exit on error), so a present binary is one that genuinely built -
    # the manifest cannot silently under-count. run_all.sh gates on this exact set (fail==0 &&
    # total==EXPECTED==pass, EXPECTED>=2 floor). gles_py (PyOpenGL) appends here once provisioned.
    : > "$bin/expected_cells"
    for c in gles_c gles_cpp gles_rust gles_py; do [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa GLES + EGL + llvmpipe DRI + LLVM) and vendor metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/glvnd" "$overlay_dir/usr/share/" 2>/dev/null || true
    # python3 interpreter for the gles_py cell (its site-packages + PyOpenGL ride /usr/lib/. above)
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-gles-compute" "$overlay_dir/opt/"
    ln -sf /opt/cpu-gles-compute/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
