#!/usr/bin/env bash
# prebuild.sh - provision the software Vulkan compute runtime (Mesa lavapipe / llvmpipe + the Vulkan
# loader) and the compiled Vulkan compute carpet binaries into the per-arch Alpine rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-vulkan-swrast
# (lavapipe, the CPU software Vulkan driver), vulkan-loader, glslang/shaderc (GLSL -> SPIR-V) and the
# runtime library closure INTO it via qemu-user-static (apk resolves every package for the TARGET arch
# on an x86 build host - no drifting URLs, no cache-miss-exit), then build the carpet cells HOST-side
# against the staged sysroot, compile the GLSL compute shaders to SPIR-V, and copy the shared-library
# closure, the lavapipe ICD metadata and the carpet binaries + runner into the overlay. Inputs are the
# base rootfs and the Alpine edge apk repos only.
#
# Toolchain: the staged Alpine gcc/g++ CANNOT run under qemu-user (gcc spawns cc1/cc1plus via
# posix_spawn, which qemu-user cannot exec), so the C/C++ cells are compiled HOST-side with a musl
# cross toolchain against the staged sysroot; apk and glslc still run under qemu-user (only gcc/cc1 was
# broken). Alpine's libvulkan.so.1 carries a `.relr.dyn` (SHT_RELR) section the older musl-cross
# binutils ld rejects ("unknown type [0x13]"); zig's bundled LLD reads it, so resolve_cc/resolve_cxx
# probe ${triple}-gcc/g++ by test-linking the staged libvulkan and fall back to `zig cc`/`zig c++
# -target <triple>` on the RELR failure. The Rust cell's ash "linked" link step also opens libvulkan,
# so its linker follows the same cc_mode (zig when GNU ld failed the probe).
#
# All backends are CPU software: lavapipe runs the Vulkan compute queue on llvmpipe (LLVM CPU JIT),
# so no host GPU is required. Alpine edge builds mesa-vulkan-swrast for all four target arches
# (x86_64 / aarch64 / riscv64 / loongarch64), so the C/C++ carpets run on-target on every arch.
#
# The Rust (ash) and Python (pyVulkan) cells run on-target too: the Rust cell is cross-compiled and
# linked against the provisioned libvulkan (ash "linked" feature) as a dynamic musl binary, and the
# Python cell runs the staged python3 interpreter with numpy (apk) and the vendored pyVulkan (cffi).
# All four language cells - C / C++ / Rust / Python - build and run on StarryOS on every arch.
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

# Kompute (C++ libkompute) is provisioned only where the user scoped it: x86_64 + aarch64. The core
# library builds cleanly for musl on those two arches (10 C++ sources, Vulkan-Hpp dynamic dispatch,
# fmt for exception messages); it is not wired for riscv64 / loongarch64.
KOMPUTE_TAG="v0.9.0"
KOMPUTE_REPO="https://github.com/KomputeProject/kompute.git"
case "$arch" in x86_64|aarch64) kompute_enabled=1 ;; *) kompute_enabled=0 ;; esac

# qemu_runner: apk (and glslc) still resolve/run against the TARGET rootfs under qemu-user - only
#              gcc/g++ were broken there (they spawn cc1/cc1plus via posix_spawn, which qemu-user
#              cannot exec), so the C/C++ cells are built HOST-side instead.
# triple:      the musl target triple for the HOST cross C/C++ compiler / linker that builds the cells.
# rust_target: the Rust cross target (cargo cross-compiles natively; only its link step needs the host
#              cross C linker, wired below by write_rust_linker).
case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     apk_arch="aarch64";     triple="aarch64-linux-musl"
                 rust_target="aarch64-unknown-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     apk_arch="riscv64";     triple="riscv64-linux-musl"
                 rust_target="riscv64gc-unknown-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      apk_arch="x86_64";      triple="x86_64-linux-musl"
                 rust_target="x86_64-unknown-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; apk_arch="loongarch64"; triple="loongarch64-linux-musl"
                 rust_target="loongarch64-unknown-linux-musl" ;;
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

# The harness injects $STARRY_OVERLAY_DIR into $base_rootfs via debugfs WITHOUT resizing, so the
# per-app image must be grown here first. The overlay carries the full mesa/lavapipe closure plus its
# LLVM runtime (~200 MiB); the stock ~2 GiB image overflows and debugfs silently truncates the
# backend libraries ("Could not allocate block"), which surfaces at runtime as "symbol not found".
# 4 GiB leaves ample headroom. Idempotent: truncate only grows, e2fsck/resize2fs are safe to re-run.
# The image stays sparse on the host.
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
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB (fs resized) for mesa/lavapipe closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa software Vulkan (lavapipe) + LLVM + the Vulkan loader + SPIR-V toolchain + build toolchain,
# all musl for the target arch. mesa-dev is intentionally NOT installed (it pulls the ~200MB
# clang-libs closure the runtime does not need). Alpine builds mesa-vulkan-swrast for every arch.
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers
          build-base glslang shaderc
          gmp mpfr4 mpc1 isl26 zlib
          # fmt: Kompute (C++) formats its exception messages through fmt::format, so libfmt + headers
          # are needed to build+link libkompute and the kompute_cpp cell (x86_64 / aarch64 only).
          fmt fmt-dev
          # Python (pyVulkan/cffi) cell runtime: python3 + numpy (float32 reference) + cffi (ABI dlopen).
          python3 py3-numpy py3-cffi)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add Vulkan stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -f "$staging_root/usr/lib/libvulkan_lvp.so" ]] || { echo "prebuild: mesa-vulkan-swrast (lavapipe) not provisioned" >&2; exit 3; }
}

# glslc (shaderc) is a self-contained ELF that does its SPIR-V codegen internally (no cc1-style
# posix_spawn), so it still runs under qemu-user against the staged shaderc - only gcc/g++ were broken.
GLSLC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/glslc" "$@"; }

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

# HOST C/C++ cross toolchains for $triple, resolved once. Alpine's libvulkan.so.1 carries a `.relr.dyn`
# (SHT_RELR) section the older musl-cross binutils ld rejects ("unknown type [0x13] section .relr.dyn");
# zig's bundled LLD reads it. Preference (mirrors the merged render/gles/opencv carpets): probe
# ${triple}-gcc / ${triple}-g++ by test-linking the staged libvulkan; on the RELR failure fall back to
# `zig cc` / `zig c++ -target <triple>` (+ STAGED libstdc++ headers/lib for the GNU std::__cxx11 ABI).
cc_mode=""; cc_gcc=""
cxx_mode=""; cxx_gpp=""; cxx_incflags=()
resolve_cc() {
    local gcc probe VK
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
    local gxxver cxxinc cxxinc_tri gpp probe VK
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
# -I$sysroot/usr/include explicitly: some host cross gcc wrappers do not add the sysroot include dir for
# angle-bracket headers, so the staged vulkan-headers (<vulkan/vulkan.h>) would otherwise be missed.
c_build() {
    local src="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cc_mode" in
        gcc) "$cc_gcc" --sysroot="$staging_root" -I"$staging_root/usr/include" -O2 -std=c11 "$src" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig cc -target "$triple" --sysroot "$staging_root" -O2 -std=c11 -I"$staging_root/usr/include" "$src" -o "$out" "$@" ;;
    esac
}

# Compile one C++ TU to a .o (zig reuses a stale object on the combined step, so split compile/link).
# -idirafter (not -I): let g++ own its C++/system headers first, then add the staged sysroot include
# (vulkan-headers) after, so <vulkan/vulkan.h> resolves even when the wrapper skips the sysroot.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -idirafter "$staging_root/usr/include" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
    esac
}
# Link C++ objects into a target ELF against the staged libvulkan. zig gets the staged libstdc++.so.6
# positionally (GNU-ABI symbols); g++ pulls its own libstdc++ implicitly. -Wl,-rpath-link resolves the
# libvulkan .so closure for the GNU-ld path. Extra positional libs (kompute .a / libfmt) pass via "$@".
cxx_link() {
    local out="$1"; shift
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig c++ -target "$triple" -o "$out" "${rpl[@]}" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-vulkan-compute"; mkdir -p "$bin/shaders"
    # Drop any stale bare libvulkan.so symlink that a prior build's Rust-link helper may have carried
    # back in via the overlay - the C/C++ cells link the versioned soname, and a dangling bare .so
    # sorts first in libpath's glob and would break the link.
    rm -f "$staging_root/usr/lib/libvulkan.so"
    local VK; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan not provisioned" >&2; exit 4; }

    # Resolve the HOST C/C++ cross toolchains once (probes the RELR wall against the staged libvulkan and
    # falls back to zig where GNU ld cannot read .relr.dyn). Shared by the C/C++ cells, the Rust linker
    # (compile_rust reads cc_mode) and kompute_cpp (compile_kompute_cpp reads cxx_mode).
    resolve_cc
    resolve_cxx

    # Vulkan compute shaders -> SPIR-V, kept next to the binaries. vulkan_c loads shaders/vadd.spv +
    # shaders/mul.spv; vulkan_cpp reuses shaders/vadd.spv. Both dispatch (N+63)/64 groups.
    for comp in "$CAR"/vulkan_c/shaders/*.comp; do
        [[ -f "$comp" ]] || continue
        GLSLC -O "$comp" -o "$bin/shaders/$(basename "${comp%.comp}").spv"
    done

    echo "prebuild: host cross-compile Vulkan carpets for $arch (lavapipe compute)"
    c_build "$CAR/vulkan_c/vulkan_c_full_api.c" "$bin/vulkan_c" "$VK" -lm
    local ocpp="$bin/vulkan_cpp.o"
    cxx_object "$CAR/vulkan_cpp/vulkan_cpp_full_api.cpp" "$ocpp"
    cxx_link "$bin/vulkan_cpp" "$ocpp" "$VK"
    rm -f "$ocpp"
    # dlopen probe (dynamic binary, does not link libvulkan): confirms the runtime dlopen path that the
    # Python (pyVulkan/cffi) binding depends on works on-target. Diagnostic only, not gated.
    c_build "$app_dir/programs/dlopen_probe.c" "$bin/dlopen_probe"
    for f in vulkan_c vulkan_cpp; do
        [[ -x "$bin/$f" ]] || { echo "prebuild: carpet $f failed to compile" >&2; exit 4; }
    done
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) native Vulkan carpet binary(ies) + run_all.sh"
}

# Fetch libkompute (pinned $KOMPUTE_TAG), build its 10 core C++ sources into a static libkompute.a with
# the HOST cross C++ toolchain resolve_cxx selected (gcc or zig, same RELR-safe path as the C++ cell),
# then cross-compile the kompute_cpp cell against it. Only the core library is built (no Python / gtest / benchmark / shader-codegen), and logging is
# disabled at the preprocessor (KOMPUTE_OPT_LOG_LEVEL_DISABLED) so no spdlog is needed; fmt is still
# required because Kompute formats its exception strings through fmt::format. Kompute drives Vulkan
# through Vulkan-Hpp's dynamic dispatch (vk::DynamicLoader dlopen()s libvulkan.so.1 at runtime), so the
# resulting cell is a dynamically linked musl binary - the same dynamic-link/dlopen model as the Rust
# and Python cells, which StarryOS supports. Scoped to x86_64 / aarch64 (set by $kompute_enabled).
KOMPUTE_DEFS=(-DKOMPUTE_OPT_LOG_LEVEL_DISABLED=1 -DKOMPUTE_OPT_USE_SPDLOG=0 -DKOMPUTE_OPT_ACTIVE_LOG_LEVEL=6
              -DVULKAN_HPP_DEFAULT_DISPATCH_LOADER_DYNAMIC=1
              -DKOMPUTE_VK_API_MAJOR_VERSION=1 -DKOMPUTE_VK_API_MINOR_VERSION=1
              -DKOMPUTE_DISABLE_VK_DEBUG_LAYERS=1)
# AR: archive the libkompute objects with the HOST cross ar (GNU-ld path) or `zig ar` (LLD path),
# matching whichever C++ toolchain resolve_cxx selected. The staged Alpine ar cannot run under
# qemu-user for the same reason gcc could not (it is part of the broken TARGET toolchain).
AR() {
    if [[ "$cxx_mode" == "gpp" ]]; then
        local ar="${cxx_gpp%g++}gcc-ar"
        [[ -x "$ar" ]] || command -v "$ar" >/dev/null 2>&1 || ar="${triple}-ar"
        "$ar" "$@"
    else
        zig ar "$@"
    fi
}
compile_kompute_cpp() {
    [[ "$kompute_enabled" -eq 1 ]] || { echo "prebuild: kompute_cpp not scoped for $arch - skipped"; return 0; }
    local bin="$staging_root/opt/cpu-vulkan-compute"; local cell="$CAR/kompute_cpp"
    local ksrc="$staging_root/tmp/kompute-src"
    rm -rf "$ksrc"
    echo "prebuild: fetch libkompute $KOMPUTE_TAG (pinned) from $KOMPUTE_REPO"
    git clone --depth 1 --branch "$KOMPUTE_TAG" "$KOMPUTE_REPO" "$ksrc" >/dev/null 2>&1 \
        || { echo "prebuild: libkompute clone failed" >&2; exit 7; }
    local inc="$ksrc/src/include"
    local kbuild="$staging_root/tmp/kompute-build"; rm -rf "$kbuild"; mkdir -p "$kbuild"
    echo "prebuild: build libkompute.a for $arch (host cross C++ toolchain, core sources only)"
    local objs=()
    for cpp in Algorithm Manager OpAlgoDispatch OpMemoryBarrier OpTensorCopy \
               OpTensorSyncDevice OpTensorSyncLocal Sequence Tensor Core; do
        cxx_object "$ksrc/src/$cpp.cpp" "$kbuild/$cpp.o" -fPIC "${KOMPUTE_DEFS[@]}" -I"$inc" \
            || { echo "prebuild: libkompute source $cpp.cpp failed to compile" >&2; exit 7; }
        objs+=("$kbuild/$cpp.o")
    done
    AR rcs "$kbuild/libkompute.a" "${objs[@]}"
    [[ -s "$kbuild/libkompute.a" ]] || { echo "prebuild: libkompute.a not produced" >&2; exit 7; }

    # kompute_cpp's own compute shaders -> SPIR-V (Kompute takes the SPIR-V words directly, so no
    # shaderc/glslang is linked into the cell). Kept in a dedicated subdir so run_all can cd there.
    mkdir -p "$bin/kompute_shaders"
    for comp in "$cell"/shaders/*.comp; do
        [[ -f "$comp" ]] || continue
        GLSLC -O "$comp" -o "$bin/kompute_shaders/$(basename "${comp%.comp}").spv"
    done

    local VK FMT; VK="$(libpath libvulkan)"; FMT="$(libpath libfmt)"
    [[ -n "$VK" ]]  || { echo "prebuild: libvulkan not provisioned for kompute_cpp" >&2; exit 7; }
    [[ -n "$FMT" ]] || { echo "prebuild: libfmt not provisioned for kompute_cpp" >&2; exit 7; }
    echo "prebuild: host cross-compile kompute_cpp cell for $arch (libkompute + Vulkan-Hpp dynamic dispatch)"
    local kobj="$kbuild/kompute_cpp.o"
    cxx_object "$cell/kompute_cpp_full_api.cpp" "$kobj" "${KOMPUTE_DEFS[@]}" -I"$inc" \
        || { echo "prebuild: kompute_cpp cell failed to compile" >&2; exit 7; }
    # libkompute.a positional AFTER the cell object (archive member pull order) and BEFORE the .so libs;
    # libfmt supplies Kompute's fmt::format symbols, libvulkan the loader entry points.
    cxx_link "$bin/kompute_cpp" "$kobj" "$kbuild/libkompute.a" "$FMT" "$VK" \
        || { echo "prebuild: kompute_cpp cell failed to link" >&2; exit 7; }
    [[ -x "$bin/kompute_cpp" ]] || { echo "prebuild: kompute_cpp binary not produced" >&2; exit 7; }
    echo "prebuild: kompute_cpp -> /opt/cpu-vulkan-compute/kompute_cpp ($(stat -c %s "$bin/kompute_cpp") bytes, dynamically linked musl; libkompute $KOMPUTE_TAG static)"
}

# write_rust_linker: cargo cross-compiles the ash cell natively, but its link step needs a HOST cross
# C linker (the staged Alpine gcc cannot spawn collect2/ld under qemu-user, the same posix_spawn wall
# as the C/C++ cells). This carpet's ash uses the "linked" feature, so the link step OPENS the staged
# libvulkan.so.1 to record it NEEDED - and libvulkan carries .relr.dyn (SHT_RELR) the old musl-cross
# binutils ld rejects. `zig cc -target <triple>` is preferred: its LLD reads .relr.dyn AND its bundled
# compiler-rt has no libgcc_s.so.1 dependency, so it links a clean DYNAMIC musl binary that records
# libvulkan.so.1 NEEDED on every arch. (A plain ${triple}-gcc is unreliable here: on this host it is a
# glibc-gcc wrapper whose rustc-driven dynamic-musl link fails with "cannot find libgcc_s.so.1", and
# where it is old musl-cross binutils it rejects .relr.dyn.) gcc is used only as a fallback when zig is
# absent. At runtime ash resolves libvulkan from the /usr/lib closure (SONAME libvulkan.so.1).
write_rust_linker() {
    local ccwrap="$1" gcc=""
    if command -v zig >/dev/null 2>&1; then
        # Strip rustc's aarch64 erratum flag --fix-cortex-a53-843419: current nightly emits it for
        # aarch64-musl, but zig cc's LLD rejects it (unsupported linker arg). gcc accepts it, so this
        # only guards the zig path.
        printf '#!/bin/sh\nn=$#; while [ $n -gt 0 ]; do a=$1; shift; case "$a" in *fix-cortex-a53-843419*) ;; *) set -- "$@" "$a";; esac; n=$((n-1)); done\nexec zig cc -target %s "$@"\n' "$triple" > "$ccwrap"; chmod +x "$ccwrap"
        echo "prebuild: Rust linker = zig cc -target $triple (compiler-rt, LLD reads .relr.dyn)"; return 0
    fi
    if command -v "${triple}-gcc" >/dev/null 2>&1; then gcc="${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then gcc="musl-gcc"; fi
    if [[ -n "$gcc" ]]; then
        printf '#!/bin/sh\nexec %s "$@"\n' "$gcc" > "$ccwrap"; chmod +x "$ccwrap"
        echo "prebuild: Rust linker = $gcc (zig absent; gcc fallback)"; return 0
    fi
    echo "prebuild: no host cross C linker for $triple (Rust cell)" >&2; return 1
}

# Cross-compile the Rust (ash) carpet on the host with the musl std target, linked by a HOST cross C
# linker (write_rust_linker), then inject the binary. No rustc/cargo runtime on-target - identical
# inject model as C/C++. ash dlopen()s libvulkan (already in the closure) at runtime. Rust is
# achievable on every arch (rustc ships all four *-unknown-linux-musl std targets), so a build failure
# here is a hard error, never a silent skip.
compile_rust() {
    local bin="$staging_root/opt/cpu-vulkan-compute"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the Rust (ash) carpet" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    # HOST cross C linker for the Rust cell (see write_rust_linker).
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    # ash "linked" emits -lvulkan; Alpine ships only libvulkan.so.<n>. Provide the bare .so symlink in a
    # BUILD-ONLY link dir (added to the Rust link search path below) so it never lands in /usr/lib -
    # putting it there would carry it into the overlay/rootfs and shadow the versioned soname the C/C++
    # cells resolve. Only the SONAME (libvulkan.so.1) is recorded as NEEDED, so runtime is unaffected.
    local vkso; vkso="$(ls "$staging_root"/usr/lib/libvulkan.so.* 2>/dev/null | head -1)"
    [[ -n "$vkso" ]] || { echo "prebuild: libvulkan.so.* not provisioned for Rust link" >&2; exit 5; }
    mkdir -p "$staging_root/opt/rustlink"
    ln -sf "$vkso" "$staging_root/opt/rustlink/libvulkan.so"
    echo "prebuild: cross-compile Rust (ash) carpet for $arch -> $rust_target (libvulkan linked via host cross linker, cc_mode=$cc_mode)"
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    # -crt-static: the musl target defaults to a fully static binary, but ash "linked" needs libvulkan
    # as a NEEDED shared object resolved at exec (the same dynamic-link model as the C/C++ cells, which
    # StarryOS supports - only dlopen is not). --offline: ash / libloading / cfg-if are vendored in the
    # cargo cache and pinned by Cargo.lock, so the build needs no (unreliable) registry access.
    ( cd "$CAR/vulkan_rust"
      env "$linkervar=$ccwrap" RUSTFLAGS="-C target-feature=-crt-static -L native=$staging_root/opt/rustlink" \
          cargo build --release --offline --target "$rust_target" 2>&1 | tail -5 )
    local rustbin="$CAR/vulkan_rust/target/$rust_target/release/vulkan_rust_full_api"
    [[ -x "$rustbin" ]] || { echo "prebuild: Rust (ash) carpet failed to cross-compile for $arch" >&2; exit 5; }
    cp "$rustbin" "$bin/vulkan_rust"
    echo "prebuild: Rust (ash) carpet -> /opt/cpu-vulkan-compute/vulkan_rust ($(stat -c %s "$rustbin") bytes, dynamically linked musl)"
}

# pyVulkan (realitix/vulkan): a pure-python (py3-none-any) cffi-ABI binding that dlopen()s
# libvulkan.so.1 at runtime - confirmed to work on StarryOS dynamic binaries (the "Dynamic loading
# not supported" error only affects fully static musl binaries). Pinned wheel from the official
# PyPI/pythonhosted host (Chinese mirrors are unreliable here), SHA-256 verified, vendored into the
# target site-packages. python3 + numpy (float32 reference) + cffi come from apk (musl). The wheel is
# arch-independent, so the same vendor step works on every arch.
PYVULKAN_WHL_URL="https://files.pythonhosted.org/packages/f7/e5/7b28a123d33fc9c3d55383628fc38322c890a97dfa2c538a7638cd71d57f/vulkan-1.3.275.1-py3-none-any.whl"
PYVULKAN_WHL_SHA256="e1e0ddf57d3a7d19f79ebf1e192b20dbd378172b027cad4f495d961b51409586"
provision_python() {
    local bin="$staging_root/opt/cpu-vulkan-compute"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 not provisioned (apk)" >&2; exit 6; }
    local sp; sp="$(ls -d "$staging_root"/usr/lib/python3.*/site-packages 2>/dev/null | head -1)"
    [[ -n "$sp" ]] || { echo "prebuild: python3 site-packages not found" >&2; exit 6; }
    local whl="$staging_root/tmp/vulkan.whl"; mkdir -p "$staging_root/tmp"
    echo "prebuild: fetch + verify pyVulkan wheel (pinned, official PyPI)"
    curl -fsSL -o "$whl" "$PYVULKAN_WHL_URL" || { echo "prebuild: pyVulkan wheel download failed" >&2; exit 6; }
    echo "$PYVULKAN_WHL_SHA256  $whl" | sha256sum -c - >/dev/null 2>&1 || { echo "prebuild: pyVulkan wheel SHA-256 mismatch" >&2; exit 6; }
    ( cd "$sp" && unzip -o -q "$whl" 'vulkan/*' )
    rm -f "$whl"
    [[ -f "$sp/vulkan/__init__.py" ]] || { echo "prebuild: pyVulkan not vendored into site-packages" >&2; exit 6; }
    # the Python carpet + a shell wrapper so run_all's run() can exec it uniformly like the native cells
    cp "$CAR/vulkan_py/vulkan_py_full_api.py" "$bin/vulkan_py.py"
    # single-threaded numpy/BLAS to match the -smp 1 target (avoid OpenBLAS core-probe / thread spawn).
    cat > "$bin/vulkan_py" <<'PYW'
#!/bin/sh
export OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 NUMEXPR_NUM_THREADS=1 MKL_NUM_THREADS=1
export HOME=/root PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
# faulthandler dumps the Python traceback on a native SIGSEGV, pinpointing the failing Vulkan call.
exec python3 -X faulthandler /opt/cpu-vulkan-compute/vulkan_py.py "$@"
PYW
    chmod +x "$bin/vulkan_py"
    echo "prebuild: Python (pyVulkan) carpet -> /opt/cpu-vulkan-compute/vulkan_py (python3 + numpy + cffi + vendored pyVulkan into $(basename "$(dirname "$sp")"))"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa lavapipe + LLVM + the Vulkan loader) and ICD metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-vulkan-compute" "$overlay_dir/opt/"
    # the python3 interpreter for the Python (pyVulkan) cell - its stdlib, numpy, cffi and the vendored
    # pyVulkan all live in the /usr/lib closure copied above; only the interpreter binary is under /usr/bin.
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    [[ -e "$overlay_dir/usr/bin/python3" ]] || { echo "prebuild: python3 interpreter not staged into overlay" >&2; exit 6; }
    ln -sf /opt/cpu-vulkan-compute/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs, python3 interpreter)"
}

# The number of language-binding cells run_all must see PASS on this arch. The four Vulkan cells
# (vulkan_c / vulkan_cpp / vulkan_rust / vulkan_py) run on every arch; kompute_cpp adds one on the two
# arches where it is scoped (x86_64 / aarch64). run_all reads this file so the gate is exact per arch.
write_expected() {
    local bin="$staging_root/opt/cpu-vulkan-compute"
    echo "$(( 4 + kompute_enabled ))" > "$bin/expected_cells"
    echo "prebuild: expected_cells = $(cat "$bin/expected_cells") for $arch"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
compile_rust
provision_python
compile_kompute_cpp
write_expected
populate_overlay
