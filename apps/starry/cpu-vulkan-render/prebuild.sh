#!/usr/bin/env bash
# prebuild.sh - provision the software Vulkan RENDER runtime (Mesa lavapipe, the CPU software Vulkan
# device reached through the vulkan-loader) and cross-compile / stage the render carpet cells into the
# per-arch Alpine rootfs. Render counterpart of cpu-vulkan-compute: instead of compute pipelines it
# builds an offscreen render pass into an R8G8B8A8_UNORM image, draws through real graphics pipelines,
# copies the image to a host-visible buffer and reads pixels back, checking each pixel against a
# closed-form reference.
#
# Portable model (same as cpu-vulkan-compute): extract the base Alpine rootfs, `apk add`
# mesa-vulkan-swrast (lavapipe ICD + libvulkan_lvp) + vulkan-loader (libvulkan.so.1) + vulkan-headers +
# build-base (staged libstdc++.so.6 + its headers, needed by the HOST C++ cross link) via qemu-user (apk
# resolves the closure for the TARGET arch), then per cell:
#   - vulkan_render_c   : HOST-cross-compile the C cell, linking the staged libvulkan. On-target on every
#     arch (mesa-vulkan-swrast + vulkan-loader present 4-arch) - a gate cell.
#   - vulkan_render_cpp : HOST-cross-compile the C++ cell, linking the staged libvulkan. Gate cell.
#   - vulkan_render_rust: cross-compile the ash cell to a dynamic-musl binary (ash::Entry::load dlopens
#     libvulkan.so.1). On-target on every arch.
#   - vulkan_render_py  : the `vulkan` cffi binding + numpy. Wired where it provisions (apk py3-cffi +
#     pip install vulkan into the staging python).
#
# Toolchain: the TARGET Alpine gcc/g++ cannot run under qemu-user (gcc spawns cc1/cc1plus via posix_spawn
# which qemu-user cannot exec), so the C/C++ cells are compiled HOST-side with a musl cross toolchain
# against the staged sysroot. apk itself still runs under qemu-user (only gcc/cc1 was broken there), so
# the -dev/library closure is still resolved for the TARGET arch. Alpine's libvulkan.so.1 carries a
# `.relr.dyn` (SHT_RELR) section the older musl-cross binutils ld rejects ("unknown type [0x13]"); zig's
# bundled LLD reads it, so both the C and C++ cells resolve their compiler through a probe that falls
# back to `zig cc`/`zig c++ -target <triple>` when the GNU ld cannot link the staged libvulkan.
#
# The SPIR-V shaders are committed (C uint32 headers for C/C++, raw .spv for Rust/Python), so no glslang
# is needed at build time. A capability manifest lists exactly the cells provisioned on this arch;
# run_all.sh gates on that set (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor: the C/C++ cells are
# the guaranteed native gate).
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR,
# STARRY_APP_DIR.
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
    # A HOST C/C++ cross toolchain (${triple}-gcc/g++ or zig) links the Alpine mesa/vulkan .so HOST-side.
    command -v "${triple}-gcc" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]] \
        || [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]] \
        || { [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; } \
        || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host cross toolchain (need ${triple}-gcc/g++ or zig) for $arch" >&2; exit 1; }
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
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for mesa/lavapipe closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa lavapipe (software Vulkan) + vulkan-loader + vulkan-headers + LLVM + build-base (staged
# libstdc++.so.6 + its headers for the HOST C++ cross link) + python, musl for the target arch.
# vulkan-headers supplies /usr/include/vulkan for the C/C++ cells.
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers
          build-base pkgconf gmp mpfr4 mpc1 isl26 zlib
          python3 py3-numpy py3-cffi py3-pip)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add software-Vulkan render stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -n "$(ls "$staging_root/usr/lib/libvulkan.so"* 2>/dev/null)" ]] \
        || { echo "prebuild: vulkan-loader (libvulkan) not provisioned" >&2; exit 3; }
    [[ -n "$(ls "$staging_root"/usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null)" ]] \
        || echo "prebuild: WARN lavapipe ICD manifest (lvp_icd*.json) not found - Phase-2 will need it"
}

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

# HOST C/C++ cross toolchains for $triple, resolved once. Alpine's libvulkan.so.1 carries a `.relr.dyn`
# (SHT_RELR) section the older musl-cross binutils ld rejects ("unknown type [0x13] section .relr.dyn");
# zig's bundled LLD reads it. Preference (mirrors the merged gles/opencv carpets): probe ${triple}-gcc /
# ${triple}-g++ by test-linking the staged libvulkan; on the RELR failure fall back to `zig cc` /
# `zig c++ -target <triple>` (+ STAGED libstdc++ headers/lib for GNU std::__cxx11 ABI mangling).
cc_mode=""; cc_gcc=""
cxx_mode=""; cxx_gpp=""; cxx_incflags=()
resolve_cc() {
    local gcc probelib probe VK
    VK="$(libpath libvulkan)"
    if command -v "${triple}-gcc" >/dev/null 2>&1; then gcc="${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then gcc="musl-gcc"
    else gcc=""; fi

    if [[ -n "$gcc" && -n "$VK" ]]; then
        probe="$(mktemp)"; printf 'int main(){return 0;}\n' > "$probe.c"
        if "$gcc" --sysroot="$staging_root" -O0 "$probe.c" -o "$probe" \
                -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$VK" >/dev/null 2>&1; then
            cc_mode="gcc"; cc_gcc="$gcc"; rm -f "$probe" "$probe.c"
            echo "prebuild: C toolchain = $gcc (--sysroot, native GNU ABI, ld reads .relr.dyn)"; return 0
        fi
        rm -f "$probe" "$probe.c"
    fi
    if command -v zig >/dev/null 2>&1; then
        cc_mode="zig"
        echo "prebuild: C toolchain = zig cc -target $triple (LLD reads .relr.dyn)"; return 0
    fi
    echo "prebuild: no host C cross toolchain for $triple (tried ${triple}-gcc, /opt, musl-gcc, zig cc)" >&2
    exit 4
}
resolve_cxx() {
    local gxxver cxxinc cxxinc_tri gpp probelib probe VK
    VK="$(libpath libvulkan)"
    gxxver="$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)"
    cxxinc="$gxxver"
    cxxinc_tri="$(ls -d "$gxxver"/*-alpine-linux-musl 2>/dev/null | head -1)"

    if command -v "${triple}-g++" >/dev/null 2>&1; then gpp="${triple}-g++"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/${triple}-cross/bin/${triple}-g++"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/cross/${triple}-cross/bin/${triple}-g++"
    else gpp=""; fi

    if [[ -n "$gpp" && -n "$VK" ]]; then
        probe="$(mktemp)"; printf 'int main(){return 0;}\n' > "$probe.cpp"
        if "$gpp" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$VK" >/dev/null 2>&1; then
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

# Compile+link one C cell. gcc pulls its own musl implicitly; zig cc links the staged libvulkan (+ musl)
# against the sysroot. -Wl,-rpath-link resolves libvulkan's own .so closure for the GNU-ld path.
c_build() {
    local src="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    # -I$sysroot/usr/include explicitly: some host cross gcc wrappers do not add the sysroot include dir
    # for angle-bracket headers, so the staged vulkan-headers (<vulkan/vulkan.h>) would be missed.
    case "$cc_mode" in
        gcc) "$cc_gcc" --sysroot="$staging_root" -I"$staging_root/usr/include" -O2 -std=c11 "$src" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig cc -target "$triple" --sysroot "$staging_root" -O2 -std=c11 -I"$staging_root/usr/include" "$src" -o "$out" "$@" ;;
    esac
}

# Compile one C++ cell to a .o first, then link - zig reuses a stale object on the combined step, so split.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    # -idirafter (not -I): let g++ own its C++/system headers first, then add the staged sysroot include
    # (vulkan-headers) after, so <vulkan/vulkan.h> resolves even when the wrapper skips the sysroot.
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -idirafter "$staging_root/usr/include" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
    esac
}
# Link one object into a target ELF against the staged libvulkan. zig gets the staged libstdc++.so.6
# positionally (GNU-ABI symbols); g++ pulls its own libstdc++ implicitly. -Wl,-rpath-link resolves the
# libvulkan .so closure for the GNU-ld path.
cxx_link() {
    local obj="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig c++ -target "$triple" "$obj" -o "$out" "${rpl[@]}" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

# vulkan_render_c: offscreen Vulkan render linking libvulkan; SPIR-V embedded via the committed
# shaders/*.h. On-target gate cell on every arch. A compile failure is a genuine breakage.
compile_c() {
    local bin="$1" VK; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan absent" >&2; exit 4; }
    echo "prebuild: host cross-compile vulkan_render_c for $arch (offscreen Vulkan render, lavapipe)"
    c_build "$CAR/vulkan_render_c/vulkan_render_c_full_api.c" "$bin/vulkan_render_c" "$VK" -lm
    [[ -x "$bin/vulkan_render_c" ]] || { echo "prebuild: vulkan_render_c failed to compile" >&2; exit 4; }
}
compile_cpp() {
    local bin="$1" VK obj; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan absent" >&2; exit 4; }
    echo "prebuild: host cross-compile vulkan_render_cpp for $arch (offscreen Vulkan render, lavapipe)"
    obj="$bin/vulkan_render_cpp.o"
    cxx_object "$CAR/vulkan_render_cpp/vulkan_render_cpp_full_api.cpp" "$obj"
    cxx_link "$obj" "$bin/vulkan_render_cpp" "$VK" -lm
    rm -f "$obj"
    [[ -x "$bin/vulkan_render_cpp" ]] || { echo "prebuild: vulkan_render_cpp failed to compile" >&2; exit 4; }
}

# scene_* : Vulkan RENDER-scene cells (C++, libvulkan). Each builds its own pipeline(s)/pass into the
# offscreen R8G8B8A8_UNORM (+ D32_SFLOAT for scene_3dmodel) target and asserts pixels against a
# closed-form reference. SPIR-V is embedded via each cell's committed shaders/*.h. Gate cells on every
# arch (native libvulkan, same closure as vulkan_render_cpp). A compile failure is a genuine breakage.
compile_scenes() {
    local bin="$1" VK obj scene; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan absent" >&2; exit 4; }
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        echo "prebuild: host cross-compile $scene for $arch (offscreen Vulkan render-scene, lavapipe)"
        obj="$bin/$scene.o"
        cxx_object "$CAR/$scene/$scene.cpp" "$obj" -I"$CAR/$scene"
        cxx_link "$obj" "$bin/$scene" "$VK" -lm
        rm -f "$obj"
        [[ -x "$bin/$scene" ]] || { echo "prebuild: $scene failed to compile" >&2; exit 4; }
    done
}

# scene_*_c : the four render-scene cells in C (libvulkan), mirroring vulkan_render_c's C idiom. Each
# reuses the same committed SPIR-V shaders/*.h the C++ scene embeds. Reference math is byte-identical to
# the C++ scenes; only the C-vs-C++ binding syntax differs. Gate cells on every arch. Compile failure =
# breakage.
compile_scenes_c() {
    local bin="$1" VK scene; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan absent" >&2; exit 4; }
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        echo "prebuild: host cross-compile ${scene}_c for $arch (offscreen Vulkan render-scene, lavapipe)"
        c_build "$CAR/${scene}_c/${scene}_c.c" "$bin/${scene}_c" -I"$CAR/${scene}_c" "$VK" -lm
        [[ -x "$bin/${scene}_c" ]] || { echo "prebuild: ${scene}_c failed to compile" >&2; exit 4; }
    done
}

# write_rust_linker: cargo cross-compiles the ash Rust cells natively, but its link step needs a musl
# cross linker. The target Alpine gcc under qemu-user cannot spawn collect2/ld, so the cargo linker points
# at the HOST cross gcc (${triple}-gcc on PATH -> /opt -> x86_64 musl-gcc -> zig cc). These cells dlopen
# libvulkan at runtime, so the link does not need the mesa .so - only a musl C linker.
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

# vulkan_render_rust: ash render cell -> dynamic-musl binary (ash::Entry::load dlopens libvulkan.so.1;
# static musl would produce empty dlopen stubs -> vacuous). Built on every arch; the libvulkan it dlopens
# rides the same /usr/lib closure.
compile_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the ash (Rust) render cell" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    local cargohome; cargohome="$(mktemp -d)"
    echo "prebuild: cross-compile vulkan_render_rust for $arch -> $rust_target (dynamic musl, ash)"
    ( cd "$CAR/vulkan_render_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
        RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --locked --target "$rust_target" 2>&1 | tail -5 ) || \
    ( cd "$CAR/vulkan_render_rust" && env "$linkervar"="$ccwrap" CARGO_HOME="$cargohome" \
        CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse RUSTFLAGS="-C target-feature=-crt-static" \
        cargo build --release --target "$rust_target" 2>&1 | tail -5 )
    local rb="$CAR/vulkan_render_rust/target/$rust_target/release/vulkan_render_rust_full_api"
    rm -rf "$cargohome"
    [[ -x "$rb" ]] || { echo "prebuild: vulkan_render_rust failed to cross-compile for $arch" >&2; exit 5; }
    cp "$rb" "$bin/vulkan_render_rust"
    echo "prebuild: vulkan_render_rust -> /opt/cpu-vulkan-render/vulkan_render_rust ($(stat -c %s "$rb") bytes, dynamic musl)"
}

# scene_*_rust : the four render-scene cells ported to ash, mirroring vulkan_render_rust's dynamic-musl
# cross-compile (ash::Entry::load dlopens libvulkan.so.1, riding the same /usr/lib closure). Each loads
# its committed SPIR-V shaders/*.spv via include_bytes! + read_spv; the closed-form references
# (Porter-Duff over, barycentric software rasterizer with Vulkan NDC z in [0,1], cubic ease, BT.601,
# DCT-II/IDCT, RLE) are behaviour-identical to the C++ scene cells. Built on every arch.
compile_scenes_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the ash (Rust) scene cells" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    local scene
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        local cargohome; cargohome="$(mktemp -d)"
        echo "prebuild: cross-compile ${scene}_rust for $arch -> $rust_target (dynamic musl, ash)"
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
        echo "prebuild: ${scene}_rust -> /opt/cpu-vulkan-render/${scene}_rust ($(stat -c %s "$rb") bytes, dynamic musl)"
    done
}

# vulkan_render_py: the `vulkan` cffi binding + numpy. cffi from apk (py3-cffi); the `vulkan` package
# from PyPI via pip into the staging python. Best-effort: where it provisions, the cell is wired.
provision_python() {
    local bin="$staging_root/opt/cpu-vulkan-render"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 absent - vulkan_render_py not wired"; return 0; }
    local sp; sp="$(ls -d "$staging_root"/usr/lib/python3*/site-packages 2>/dev/null | head -1 || true)"
    [[ -n "$sp" ]] || { echo "prebuild: no site-packages - vulkan_render_py not wired"; return 0; }
    if [[ ! -d "$sp/vulkan" ]]; then
        echo "prebuild: pip install vulkan into staging python for $arch (best-effort)"
        QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" HOME=/tmp \
            "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -m pip install --root "$staging_root" \
            --prefix /usr --no-warn-script-location vulkan 2>&1 | tail -4 || true
    fi
    [[ -d "$sp/vulkan" ]] || { echo "prebuild: vulkan cffi package absent for $arch - vulkan_render_py not wired"; return 0; }
    cp "$CAR/vulkan_render_py/vulkan_render_py_full_api.py" "$bin/vulkan_render_py.py"
    cat > "$bin/vulkan_render_py" <<'PYW'
#!/bin/sh
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
export VK_ICD_FILENAMES="$ICD"
export VK_DRIVER_FILES="$ICD"
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-vulkan-render/vulkan_render_py.py "$@"
PYW
    chmod +x "$bin/vulkan_render_py"
    echo "prebuild: vulkan_render_py -> /opt/cpu-vulkan-render/vulkan_render_py (python3 + numpy + vulkan cffi)"

    # scene_*_py: the four render-scene cells ported to the `vulkan` cffi binding + numpy, staged where
    # the vulkan cffi package resolved (same honest-skip gate as vulkan_render_py). Each loads its
    # committed SPIR-V shaders/*.spv at runtime relative to its own script, so stage each cell's shaders/
    # dir alongside it. The numpy closed-form references mirror the C++ scene cells behaviour-identically.
    local scene
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        # each scene_*_py loads shaders/*.spv relative to its own script, so give it a private dir with
        # the script + its shaders/ (a shared $bin/shaders would collide across the four scenes).
        rm -rf "$bin/${scene}_py.d"; mkdir -p "$bin/${scene}_py.d"
        cp "$CAR/${scene}_py/${scene}_py.py" "$bin/${scene}_py.d/${scene}_py.py"
        cp -a "$CAR/${scene}_py/shaders" "$bin/${scene}_py.d/shaders"
        cat > "$bin/${scene}_py" <<PYW
#!/bin/sh
ICD=\$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
export VK_ICD_FILENAMES="\$ICD"
export VK_DRIVER_FILES="\$ICD"
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-vulkan-render/${scene}_py.d/${scene}_py.py "\$@"
PYW
        chmod +x "$bin/${scene}_py"
        echo "prebuild: ${scene}_py -> /opt/cpu-vulkan-render/${scene}_py (python3 + numpy + vulkan cffi)"
    done
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-vulkan-render"; mkdir -p "$bin"
    resolve_cc
    resolve_cxx
    compile_c "$bin"
    compile_cpp "$bin"
    compile_scenes "$bin"
    compile_scenes_c "$bin"
    compile_rust "$bin"
    compile_scenes_rust "$bin"
    provision_python
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

populate_overlay() {
    local bin="$staging_root/opt/cpu-vulkan-render"
    # capability manifest: exactly the cells provisioned on this arch (each native build hard-fails, so a
    # present binary genuinely built). run_all.sh gates on this set (>=1 floor: the C/C++ cells).
    : > "$bin/expected_cells"
    for c in vulkan_render_c vulkan_render_cpp \
             scene_2dui scene_3dmodel scene_anim scene_codec \
             scene_2dui_c scene_3dmodel_c scene_anim_c scene_codec_c \
             vulkan_render_rust scene_2dui_rust scene_3dmodel_rust scene_anim_rust scene_codec_rust \
             vulkan_render_py scene_2dui_py scene_3dmodel_py scene_anim_py scene_codec_py; do
        [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-vulkan-render" "$overlay_dir/opt/"
    ln -sf /opt/cpu-vulkan-render/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
echo "prebuild: cpu-vulkan-render overlay ready for $arch"
