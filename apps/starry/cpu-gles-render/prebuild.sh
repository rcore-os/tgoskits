#!/usr/bin/env bash
# prebuild.sh - provision the software OpenGL ES RENDER runtime (Mesa llvmpipe, the CPU software
# GL driver reached through surfaceless EGL) and cross-compile / stage the render carpet cells into the
# per-arch Alpine rootfs. Render counterpart of cpu-opengl-compute: instead of compute shaders it draws
# into an off-screen FBO and reads pixels back, checking each pixel against a closed-form reference.
#
# Portable model (same as cpu-opengl-compute): extract the base Alpine rootfs, `apk add` mesa-gl /
# mesa-egl / mesa-gles / mesa-dri-gallium + toolchain via qemu-user (apk resolves for the TARGET arch),
# then per cell:
#   - gles_render_cpp : cross-compile the C++ cell with the staging g++ (OpenGL ES 3.1, direct libGLESv2,
#     links libGLESv2+libEGL). On-target on every arch (mesa-gles/egl present 4-arch) - the gate cell.
#   - gles_render_rust: cross-compile the glow cell to a dynamic-musl binary (khronos-egl dlopens
#     libEGL). On-target on every arch.
#   - gles_render_py  : PyOpenGL (OpenGL.EGL + OpenGL.GLES3) surfaceless ES render (py3-opengl from apk).
# A capability manifest lists exactly the cells provisioned on this arch; run_all.sh gates on that set
# (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor: the C++ cell is the guaranteed native gate).
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR,
# STARRY_APP_DIR. The GL/EGL/KHR headers are vendored under programs/headers.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
CAR="$app_dir/programs/carpets"

# qemu_runner: apk still resolves the TARGET rootfs under qemu-user (only gcc/cc1 was broken there).
# triple:      the musl target triple for the HOST cross C/C++ compiler that builds the cells.
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
    # HOST pkgconf resolves the staged glesv2/egl .pc against the staging root (target gcc under qemu-user
    # cannot spawn cc1). A HOST C++ cross toolchain (${triple}-g++ or zig c++) links the Alpine mesa .so.
    command -v pkgconf >/dev/null 2>&1 || command -v pkg-config >/dev/null 2>&1 \
        || { echo "prebuild: host pkgconf/pkg-config required" >&2; exit 1; }
    command -v "${triple}-g++" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]] \
        || [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]] || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host C++ cross toolchain (need ${triple}-g++ or zig) for $arch" >&2; exit 1; }
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
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
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for mesa/llvmpipe closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa desktop-GL + GLES + EGL + gallium DRI (llvmpipe) + LLVM + build-base (staged libstdc++.so.6 +
# its headers, needed by the HOST C++ cross link) + python, musl for the target arch. mesa-dev is staged
# so glesv2.pc / egl.pc exist for the HOST pkgconf run that resolves the cell link flags.
GPU_PKGS=(musl mesa-gl mesa-egl mesa-gles mesa-dev mesa-dri-gallium
          build-base pkgconf gmp mpfr4 mpc1 isl26 zlib
          python3 py3-numpy)
PY_BINDING_PKGS=(py3-opengl)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add desktop-GL render stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -n "$(ls "$staging_root/usr/lib/libEGL.so"* 2>/dev/null)" ]] \
        || { echo "prebuild: mesa-egl (libEGL) not provisioned" >&2; exit 3; }
    # best-effort PyOpenGL (gles_render_py). Where apk cannot resolve it, the cell is not wired.
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${PY_BINDING_PKGS[@]}"; then
        echo "prebuild: PyOpenGL (gles_render_py) provisioned for $arch"
    else
        echo "prebuild: py3-opengl unavailable for $arch (gles_render_py not wired this arch)"
    fi
}

# HOST pkgconf resolving the staged glesv2/egl .pc against the staging root (target gcc under qemu-user
# cannot spawn cc1, so linking runs HOST-side against the staged sysroot).
host_pkgconf() { if command -v pkgconf >/dev/null 2>&1; then echo pkgconf; else echo pkg-config; fi; }
PKGCFG() { PKG_CONFIG_SYSROOT_DIR="$staging_root" PKG_CONFIG_LIBDIR="$staging_root/usr/lib/pkgconfig" \
           "$(host_pkgconf)" "$@"; }
libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

# The HOST C++ cross toolchain for $triple, resolved once. The C++ cells link Alpine's mesa libGLESv2 /
# libEGL, which carry a `.relr.dyn` (SHT_RELR) section older musl-cross binutils ld rejects; zig's bundled
# LLD reads it. Preference order (mirrors the merged opencv carpet):
#   1) ${triple}-g++ (PATH or /opt) - native GNU ABI; used only if its ld can actually link a mesa .relr.dyn .so.
#   2) zig c++ -target <triple> + STAGED libstdc++ headers/lib - LLD reads .relr.dyn, GNU (std::__cxx11) ABI.
cxx_mode=""; cxx_gpp=""; cxx_incflags=()
resolve_cxx() {
    local gxxver cxxinc cxxinc_tri gpp probelib
    gxxver="$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)"
    cxxinc="$gxxver"
    cxxinc_tri="$(ls -d "$gxxver"/*-alpine-linux-musl 2>/dev/null | head -1)"

    if command -v "${triple}-g++" >/dev/null 2>&1; then gpp="${triple}-g++"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/${triple}-cross/bin/${triple}-g++"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/cross/${triple}-cross/bin/${triple}-g++"
    else gpp=""; fi

    # Prefer g++ only if its ld can actually link an Alpine .relr.dyn mesa .so - probe against libGLESv2.
    probelib="$(libpath libGLESv2)"
    if [[ -n "$gpp" && -n "$probelib" ]]; then
        local probe; probe="$(mktemp)"
        printf 'int main(){return 0;}\n' > "$probe.cpp"
        if "$gpp" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -L"$staging_root/usr/lib" -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" \
                -lGLESv2 >/dev/null 2>&1; then
            cxx_mode="gpp"; cxx_gpp="$gpp"; rm -f "$probe" "$probe.cpp"
            echo "prebuild: C++ toolchain = $gpp (--sysroot, native GNU ABI, ld reads .relr.dyn)"; return 0
        fi
        rm -f "$probe" "$probe.cpp"
    fi

    if command -v zig >/dev/null 2>&1; then
        [[ -d "$cxxinc" ]] || { echo "prebuild: no staged libstdc++ headers for zig c++ path" >&2; exit 4; }
        cxx_mode="zig"; cxx_incflags=(-nostdinc++ -isystem "$cxxinc")
        [[ -n "$cxxinc_tri" ]] && cxx_incflags+=(-isystem "$cxxinc_tri")
        cxx_incflags+=(-idirafter "$staging_root/usr/include")
        echo "prebuild: C++ toolchain = zig c++ -target $triple (staged libstdc++ headers, LLD reads .relr.dyn)"
        return 0
    fi

    echo "prebuild: no host C++ cross toolchain for $triple (tried ${triple}-g++, /opt, zig c++)" >&2
    exit 4
}

# Compile one C++ cell to a .o first, then link - zig reuses a stale object on the combined step, so split.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
    esac
}

# Link one object into a target ELF against the staged mesa. zig gets the staged libstdc++.so.6 positionally
# (GNU-ABI symbols); g++ pulls its own libstdc++ implicitly. -Wl,-rpath-link resolves the mesa .so closure.
cxx_link() {
    local obj="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig c++ -target "$triple" "$obj" -o "$out" "${rpl[@]}" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

# write_rust_linker: cargo cross-compiles the glow/khronos-egl Rust cells natively, but its link step needs
# a musl cross linker. The target Alpine gcc under qemu-user cannot spawn collect2/ld, so the cargo linker
# points at the HOST cross gcc (${triple}-gcc on PATH -> /opt -> zig cc -> x86_64 musl-gcc). These cells
# dlopen libEGL at runtime, so the link does not need the mesa .so - only a musl C linker.
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
        echo "prebuild: no host cross C linker for $triple (Rust cells)" >&2; return 1
    fi
    chmod +x "$ccwrap"
}

# gles_render_cpp: OpenGL ES 3.1 render via surfaceless EGL; GLES entry points come directly from libGLESv2
# (no loader). On-target gate cell on every arch. A compile failure is a genuine breakage. Built HOST-side
# with the cross toolchain against the staged sysroot; glesv2/egl flags from the HOST pkgconf.
compile_cpp() {
    local bin="$1" hdr="$app_dir/programs/headers" cflags libs obj
    cflags="$(PKGCFG --cflags glesv2 egl)"
    libs="$(PKGCFG --libs glesv2 egl)"
    echo "prebuild: host cross-compile gles_render_cpp for $arch (surfaceless EGL, OpenGL ES 3.1 render)"
    obj="$bin/gles_render_cpp.o"
    # shellcheck disable=SC2086
    cxx_object "$CAR/gles_render_cpp/gles_render_cpp_full_api.cpp" "$obj" -I"$hdr" $cflags
    # shellcheck disable=SC2086
    cxx_link "$obj" "$bin/gles_render_cpp" $libs -lm
    rm -f "$obj"
    [[ -x "$bin/gles_render_cpp" ]] || { echo "prebuild: gles_render_cpp failed to compile/link" >&2; exit 4; }
}

# scene_* : real-scenario C++ render cells (2D UI compositing / 3D model / animation / streaming codec).
# Same surfaceless-EGL / direct-libGLESv2 path as gles_render_cpp; each builds unconditionally on every
# arch (so each is always in the manifest). A compile failure is a genuine breakage.
compile_scenes() {
    local bin="$1" hdr="$app_dir/programs/headers" cflags libs obj
    cflags="$(PKGCFG --cflags glesv2 egl)"
    libs="$(PKGCFG --libs glesv2 egl)"
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        echo "prebuild: host cross-compile $scene for $arch (surfaceless EGL, OpenGL ES 3.1 render)"
        obj="$bin/$scene.o"
        # shellcheck disable=SC2086
        cxx_object "$CAR/$scene/$scene.cpp" "$obj" -I"$hdr" $cflags
        # shellcheck disable=SC2086
        cxx_link "$obj" "$bin/$scene" $libs -lm
        rm -f "$obj"
        [[ -x "$bin/$scene" ]] || { echo "prebuild: $scene failed to compile/link" >&2; exit 4; }
    done
}

# scene_*_rust : the four real-scenario render cells ported to glow + khronos-egl(dynamic), mirroring
# gles_render_rust's dynamic-musl cross-compile (each dlopens libEGL, riding the same /usr/lib closure).
# The closed-form references (Porter-Duff over, software rasterizer barycentric+1/w, cubic ease, BT.601,
# DCT-II/IDCT, RLE) are ported behaviour-identically to the C++ scene cells. Built on every arch.
compile_scenes_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the glow (Rust) scene cells" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        local cargohome; cargohome="$(mktemp -d)"
        echo "prebuild: cross-compile ${scene}_rust for $arch -> $rust_target (dynamic musl, glow+khronos-egl)"
        ( cd "$CAR/${scene}_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
            RUSTFLAGS="-C target-feature=-crt-static" \
            cargo build --release --locked --target "$rust_target" 2>&1 | tail -5 ) || \
        ( cd "$CAR/${scene}_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse RUSTFLAGS="-C target-feature=-crt-static" \
            cargo build --release --target "$rust_target" 2>&1 | tail -5 )
        local rb="$CAR/${scene}_rust/target/$rust_target/release/${scene}_rust"
        rm -rf "$cargohome"
        [[ -x "$rb" ]] || { echo "prebuild: ${scene}_rust failed to cross-compile for $arch" >&2; exit 5; }
        cp "$rb" "$bin/${scene}_rust"
        echo "prebuild: ${scene}_rust -> /opt/cpu-gles-render/${scene}_rust ($(stat -c %s "$rb") bytes, dynamic musl)"
    done
}

# gles_render_rust: glow + khronos-egl(dynamic) render cell -> dynamic-musl binary (static musl stubs
# dlopen -> vacuous). Built on every arch; the libEGL it dlopens rides the same /usr/lib closure.
compile_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the glow (Rust) render cell" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    local cargohome; cargohome="$(mktemp -d)"
    echo "prebuild: cross-compile gles_render_rust for $arch -> $rust_target (dynamic musl, glow+khronos-egl)"
    ( cd "$CAR/gles_render_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --locked --target "$rust_target" 2>&1 | tail -5 ) || \
    ( cd "$CAR/gles_render_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --target "$rust_target" 2>&1 | tail -5 )
    local rb="$CAR/gles_render_rust/target/$rust_target/release/gles_render_rust_full_api"
    rm -rf "$cargohome"
    [[ -x "$rb" ]] || { echo "prebuild: gles_render_rust failed to cross-compile for $arch" >&2; exit 5; }
    cp "$rb" "$bin/gles_render_rust"
    echo "prebuild: gles_render_rust -> /opt/cpu-gles-render/gles_render_rust ($(stat -c %s "$rb") bytes, dynamic musl)"
}

# gles_render_py: PyOpenGL (OpenGL.EGL + OpenGL.GLES3) surfaceless ES render. Wired where py3-opengl resolved.
provision_python() {
    local bin="$staging_root/opt/cpu-gles-render"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 absent - gles_render_py not wired"; return 0; }
    ls -d "$staging_root"/usr/lib/python3*/site-packages/OpenGL >/dev/null 2>&1 \
        || { echo "prebuild: PyOpenGL absent for $arch - gles_render_py not wired"; return 0; }
    cp "$CAR/gles_render_py/gles_render_py_full_api.py" "$bin/gles_render_py.py"
    cat > "$bin/gles_render_py" <<'PYW'
#!/bin/sh
# PyOpenGL loads GL entry points via GLX by default; StarryOS is headless (no X11), so pin the EGL
# platform so PyOpenGL resolves functions through eglGetProcAddress against the surfaceless context.
export PYOPENGL_PLATFORM=egl
export EGL_PLATFORM=surfaceless
export GALLIUM_DRIVER=llvmpipe
export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-gles-render/gles_render_py.py "$@"
PYW
    chmod +x "$bin/gles_render_py"
    echo "prebuild: gles_render_py -> /opt/cpu-gles-render/gles_render_py (python3 + numpy + PyOpenGL ES)"

    # scene_*_py: the four real-scenario render cells ported to PyOpenGL (OpenGL.EGL + OpenGL.GLES3 +
    # numpy references), staged where PyOpenGL resolved (same honest-skip gate as gles_render_py). The
    # numpy closed-form references mirror the C++ scene cells behaviour-identically.
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        cp "$CAR/${scene}_py/${scene}_py.py" "$bin/${scene}_py.py"
        cat > "$bin/${scene}_py" <<PYW
#!/bin/sh
export PYOPENGL_PLATFORM=egl
export EGL_PLATFORM=surfaceless
export GALLIUM_DRIVER=llvmpipe
export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-gles-render/${scene}_py.py "\$@"
PYW
        chmod +x "$bin/${scene}_py"
        echo "prebuild: ${scene}_py -> /opt/cpu-gles-render/${scene}_py (python3 + numpy + PyOpenGL ES)"
    done
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-gles-render"; mkdir -p "$bin"
    resolve_cxx
    compile_cpp "$bin"
    compile_scenes "$bin"
    compile_rust "$bin"
    compile_scenes_rust "$bin"
    provision_python
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

populate_overlay() {
    local bin="$staging_root/opt/cpu-gles-render"
    # capability manifest: exactly the cells provisioned on this arch (each build hard-fails, so a
    # present binary genuinely built). run_all.sh gates on this set (>=1 floor: gles_render_cpp).
    : > "$bin/expected_cells"
    for c in gles_render_cpp scene_2dui scene_3dmodel scene_anim scene_codec \
             gles_render_rust scene_2dui_rust scene_3dmodel_rust scene_anim_rust scene_codec_rust \
             gles_render_py scene_2dui_py scene_3dmodel_py scene_anim_py scene_codec_py; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/glvnd" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-gles-render" "$overlay_dir/opt/"
    ln -sf /opt/cpu-gles-render/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
echo "prebuild: cpu-gles-render overlay ready for $arch"
