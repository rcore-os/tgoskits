#!/usr/bin/env bash
# prebuild.sh - provision the OpenCL runtime (mesa rusticl / pocl) and compiled carpet binaries
# into the per-arch Alpine rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` the build toolchain
# and OpenCL packages INTO it via qemu-user-static, cross-compile the carpet sources against the
# provisioned musl headers/libraries with the target gcc under qemu-user, then copy the shared
# library closure and carpet binaries + runner into the overlay.
# No host-absolute paths, no prebuilt images - inputs are the base rootfs, the Alpine edge apk
# repos and the app's own programs/ sources.
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
# triple:      the musl target triple for the HOST cross C/C++ compiler that builds the cells.
# rust_target: the opencl3 (Rust) cell's cross target; cargo cross-compiles fine, only its link step
#              needs a HOST cross C linker (write_rust_linker). Built as a dynamic musl binary (opencl3
#              links -lOpenCL, so a static musl binary's stubbed dlopen/link would be a vacuous
#              no-adapter green; -crt-static is disabled below). Only built where libOpenCL is present.
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
    # A HOST C cross toolchain builds the C cells (staged Alpine gcc's cc1 cannot run under qemu-user:
    # gcc spawns cc1 via posix_spawn which qemu-user cannot exec). A HOST C++ cross toolchain
    # (${triple}-g++ or zig c++) links the Alpine libstdc++-built .so for the C++ cell.
    command -v "${triple}-gcc" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]] \
        || [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]] \
        || { [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; } || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host C cross toolchain (need ${triple}-gcc or zig) for $arch" >&2; exit 1; }
    command -v "${triple}-g++" >/dev/null 2>&1 || [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]] \
        || [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]] || command -v zig >/dev/null 2>&1 \
        || { echo "prebuild: no host C++ cross toolchain (need ${triple}-g++ or zig) for $arch" >&2; exit 1; }
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

# The opencl-icd-loader + mesa-rusticl (OpenCL over llvmpipe) pull in LLVM22, libclang-cpp, and the
# full mesa closure. On x64 pocl also brings LLVM20+hwloc. Together this exceeds the stock ~2 GiB
# image; 4 GiB leaves ample headroom. Idempotent: truncate only grows, e2fsck/resize2fs safe to re-run.
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
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB (fs resized) for OpenCL closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# build toolchain + OpenCL headers, all musl for the target arch. python3 + py3-numpy carry the
# PyOpenCL cell's interpreter and float32 reference (available on all four Alpine arches, no libOpenCL
# dependency of their own).
GPU_PKGS=(musl build-base opencl-headers gmp mpfr4 mpc1 isl26 zlib python3 py3-numpy)
# Optional: mesa-rusticl (OpenCL over llvmpipe) + ICD loader. Not available on every Alpine arch
# (absent on riscv64 as of Alpine edge 2026-07); absent arches are served by pocl via POCL_PREBUILT.
GPU_PKGS_OPT=(mesa-rusticl opencl-icd-loader)
# PyOpenCL binding: py3-opencl is a musl-native C extension (_cl.*.so) Alpine builds for all four
# target arches; it needs libOpenCL.so.1 at runtime (provided by rusticl on x64/aa or pocl on rv/la),
# so it is provisioned best-effort and the PyOpenCL cell is only wired where libOpenCL is present.
PY_BINDING_PKGS=(py3-opencl)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add build toolchain (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    # best-effort: mesa-rusticl (OpenCL over llvmpipe) is not available on every Alpine arch
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${GPU_PKGS_OPT[@]}"; then
        echo "prebuild: OpenCL rusticl provisioned for $arch"
    else
        echo "prebuild: OpenCL rusticl unavailable for $arch (upstream Alpine arch gap)"
    fi
    # best-effort: PyOpenCL binding (py3-opencl). Its so:libOpenCL.so.1 runtime dep is satisfied by
    # rusticl (added above) on x64/aa or pocl (folded later) on rv/la; where apk cannot resolve it the
    # PyOpenCL cell is simply not wired (finalize_dynamic_cells gates on the _cl extension being present).
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${PY_BINDING_PKGS[@]}"; then
        echo "prebuild: PyOpenCL binding (py3-opencl) provisioned for $arch"
    else
        echo "prebuild: py3-opencl unavailable for $arch (apk could not resolve; PyOpenCL cell not wired this arch)"
    fi
}

# Compiler selection: HOST cross toolchain, both for the native arch AND foreign arches.
#
# The staged Alpine gcc/g++ cannot compile under qemu-user: gcc's driver spawns cc1/cc1plus (and
# collect2/ld) via posix_spawn, which qemu-user cannot exec (it fails with "cannot execute cc1:
# posix_spawn: No such file"), so the whole staged-gcc-under-qemu path is broken on every arch. The
# fix is to compile HOST-side with a musl cross toolchain (${triple}-gcc/g++, native binaries whose
# own cc1 resolves natively) plus --sysroot pointing at the Alpine staging tree, so every built-in
# header/library path resolves against the staged Alpine sysroot and the emitted ELF is the target
# arch with the /lib/ld-musl-<arch>.so.1 interpreter Alpine expects. Alpine ships no -lOpenCL .so
# symlink, so the cells link the full libOpenCL soname positionally (resolved by libpath).
#
# GCC(): the host C compile+link for the C cells (opencl_c, clvk_c) and the pocl svml stub. The C cells
# LINK the Alpine libOpenCL.so which carries a `.relr.dyn` (SHT_RELR) section old musl-cross binutils ld
# (~2.36) rejects ("unknown type [0x13] section .relr.dyn"), so resolve_cc probes the cross gcc's ld
# against the staged libOpenCL and falls back to `zig cc` (bundled LLD reads RELR) when it fails. Both
# paths use --sysroot at the staging tree + explicit -I usr/include (some packaged musl-cross
# toolchains resolve headers from a built-in <sysroot>/include layout, missing Alpine's usr/include).
# GPP(): the host C++ compile+link for the opencl_cpp cell - same RELR handling via resolve_cxx, plus
# STAGED libstdc++ headers/lib for correct std::__cxx11 GNU-ABI mangling (see resolve_cxx).
cc_mode=""; cc_gcc=""
resolve_cc() {
    [[ -n "$cc_mode" ]] && return 0
    local gcc="" probelib probe
    if command -v "${triple}-gcc" >/dev/null 2>&1; then gcc="${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]]; then gcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then gcc="musl-gcc"; fi
    # Prefer the cross gcc only if its ld can actually link a staged Alpine .relr.dyn .so.
    probelib="$(libpath libOpenCL)"
    if [[ -n "$gcc" && -n "$probelib" ]]; then
        probe="$(mktemp)"; printf 'int main(){return 0;}\n' > "$probe.c"
        if "$gcc" --sysroot="$staging_root" -O0 "$probe.c" -o "$probe" \
                -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$probelib" >/dev/null 2>&1; then
            cc_mode="gcc"; cc_gcc="$gcc"; rm -f "$probe" "$probe.c"
            echo "prebuild: C toolchain = $gcc (--sysroot, native ABI, ld reads .relr.dyn)"; return 0
        fi
        rm -f "$probe" "$probe.c"
    fi
    if command -v zig >/dev/null 2>&1; then
        cc_mode="zig"
        echo "prebuild: C toolchain = zig cc -target $triple (LLD reads .relr.dyn)"; return 0
    fi
    [[ -n "$gcc" ]] && { cc_mode="gcc"; cc_gcc="$gcc"; echo "prebuild: C toolchain = $gcc (no zig; RELR unverified)"; return 0; }
    echo "prebuild: no host C cross toolchain for $triple (tried ${triple}-gcc, /opt, musl-gcc, zig cc)" >&2; return 1
}
GCC() { resolve_cc || exit 1
    case "$cc_mode" in
        gcc) "$cc_gcc" --sysroot="$staging_root" -I"$staging_root/usr/include" \
             -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$@" ;;
        zig) zig cc -target "$triple" --sysroot="$staging_root" -I"$staging_root/usr/include" \
             -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$@" ;;
    esac
}

cxx_mode=""; cxx_gpp=""; cxx_incflags=()
resolve_cxx() {
    [[ -n "$cxx_mode" ]] && return 0
    local gxxver cxxinc cxxinc_tri gpp probelib
    gxxver="$(ls -d "$staging_root"/usr/include/c++/* 2>/dev/null | head -1)"
    cxxinc="$gxxver"
    cxxinc_tri="$(ls -d "$gxxver"/*-alpine-linux-musl 2>/dev/null | head -1)"

    if command -v "${triple}-g++" >/dev/null 2>&1; then gpp="${triple}-g++"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/${triple}-cross/bin/${triple}-g++"
    elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-g++" ]]; then gpp="/opt/cross/${triple}-cross/bin/${triple}-g++"
    else gpp=""; fi

    # Prefer g++ only if its ld can actually link an Alpine .relr.dyn .so - probe against staged libOpenCL.
    probelib="$(libpath libOpenCL)"
    if [[ -n "$gpp" && -n "$probelib" ]]; then
        local probe; probe="$(mktemp)"
        printf 'int main(){return 0;}\n' > "$probe.cpp"
        if "$gpp" --sysroot="$staging_root" -O0 "$probe.cpp" -o "$probe" \
                -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" "$probelib" >/dev/null 2>&1; then
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

# Compile+link the opencl_cpp cell. zig reuses a stale object on a combined compile+link step, so split
# into a .o then link; zig gets the staged libstdc++.so.6 positionally (GNU-ABI symbols), g++ pulls its
# own libstdc++ implicitly. Extra args (defines / the libOpenCL soname) pass through to both steps as
# appropriate: compile flags to cxx_object, link libs to cxx_link.
cxx_object() {
    local src="$1" obj="$2"; shift 2
    # -I$staging_root/usr/include: the Alpine CL/opencl.hpp lives there; explicit for the same
    # packaged-toolchain sysroot layout reason as GCC (zig's cxx_incflags already -idirafter it).
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" -I"$staging_root/usr/include" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
        zig) zig c++ -target "$triple" "${cxx_incflags[@]}" -O2 -std=c++17 "$@" -c "$src" -o "$obj" ;;
    esac
}
cxx_link() {
    local obj="$1" out="$2"; shift 2
    local rpl=(-Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib")
    case "$cxx_mode" in
        gpp) "$cxx_gpp" --sysroot="$staging_root" "$obj" -o "$out" "${rpl[@]}" "$@" ;;
        zig) zig c++ -target "$triple" "$obj" -o "$out" "${rpl[@]}" "$@" "$staging_root/usr/lib/libstdc++.so.6" ;;
    esac
}

# write_rust_linker: cargo cross-compiles the opencl3 cell natively, but its link step needs a HOST
# cross C linker (the target Alpine gcc under qemu-user cannot spawn collect2/ld). Unlike carpets whose
# Rust cells dlopen their lib, opencl3 LINKS -lOpenCL, so the final link consumes the Alpine
# libOpenCL.so which carries a `.relr.dyn` (SHT_RELR) section. Old musl-cross binutils ld (~2.36)
# rejects it ("unknown type [0x13] section .relr.dyn"), so PREFER `zig cc` (bundled LLD reads RELR);
# fall back to a musl-cross gcc only where zig is absent (arches whose libOpenCL has no RELR). The Rust
# cell SOURCE is untouched; only the linker env changes.
write_rust_linker() {
    local ccwrap="$1" hostcc=""
    if command -v zig >/dev/null 2>&1; then
        # rustc's aarch64 target spec passes -Wl,--fix-cortex-a53-843419, which zig's LLD driver rejects
        # ("unsupported linker arg"); strip it (the erratum workaround is irrelevant to a QEMU guest).
        cat > "$ccwrap" <<WRAP
#!/bin/sh
args=""
for a in "\$@"; do
  case "\$a" in -Wl,--fix-cortex-a53-843419|--fix-cortex-a53-843419) continue ;; esac
  args="\$args \"\$a\""
done
eval exec zig cc -target $triple \$args
WRAP
    else
        if command -v "${triple}-gcc" >/dev/null 2>&1; then hostcc="${triple}-gcc"
        elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then hostcc="/opt/${triple}-cross/bin/${triple}-gcc"
        elif [[ -x "/opt/cross/${triple}-cross/bin/${triple}-gcc" ]]; then hostcc="/opt/cross/${triple}-cross/bin/${triple}-gcc"
        elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then hostcc="musl-gcc"; fi
        [[ -n "$hostcc" ]] || { echo "prebuild: no host cross C linker for $triple (Rust cell)" >&2; return 1; }
        printf '#!/bin/sh\nexec %s "$@"\n' "$hostcc" > "$ccwrap"
    fi
    chmod +x "$ccwrap"
}

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

# Cross-compile the opencl3 (Rust) cell to a dynamic musl binary linked against the provisioned
# libOpenCL and inject it. cargo cross-compiles fine; its link step uses the HOST cross C linker
# (write_rust_linker) - the target Alpine gcc under qemu-user cannot spawn collect2/ld. Called only
# where libOpenCL is present (so it rides the same arch availability as opencl_c/opencl_cpp).
compile_rust() {
    local bin="$1"
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required for the opencl3 (Rust) cell" >&2; exit 5; }
    rustup target list --installed 2>/dev/null | grep -qx "$rust_target" || rustup target add "$rust_target" >/dev/null 2>&1 || true
    local ccwrap="$staging_root/opt/rust-cc"; mkdir -p "$staging_root/opt"
    write_rust_linker "$ccwrap" || exit 5
    # opencl3 links -lOpenCL; Alpine ships only libOpenCL.so.<n>, so give the bare .so symlink in a
    # BUILD-ONLY dir (added to the Rust link search path) - it never enters the overlay/rootfs.
    local clso; clso="$(ls "$staging_root"/usr/lib/libOpenCL.so.* 2>/dev/null | head -1)"
    [[ -n "$clso" ]] || { echo "prebuild: libOpenCL.so.* not provisioned for Rust link" >&2; exit 5; }
    mkdir -p "$staging_root/opt/rustlink"; ln -sf "$clso" "$staging_root/opt/rustlink/libOpenCL.so"
    echo "prebuild: cross-compile opencl3 (Rust) cell for $arch -> $rust_target (libOpenCL linked, host cross C linker)"
    local linkervar; linkervar="CARGO_TARGET_$(printf '%s' "$rust_target" | tr 'a-z.-' 'A-Z__')_LINKER"
    # -crt-static disabled: opencl3 needs libOpenCL as a NEEDED shared object at exec (dynamic-link
    # model, like the C/C++ cells). --offline: opencl3/cl3 are vendored and pinned by Cargo.lock.
    ( cd "$CAR/opencl_rust"
      env "$linkervar=$ccwrap" RUSTFLAGS="-C target-feature=-crt-static -L native=$staging_root/opt/rustlink" \
          cargo build --release --offline --target "$rust_target" 2>&1 | tail -5 )
    local rustbin="$CAR/opencl_rust/target/$rust_target/release/opencl_rust_full_api"
    [[ -x "$rustbin" ]] || { echo "prebuild: opencl3 (Rust) cell failed to cross-compile for $arch" >&2; exit 5; }
    cp "$rustbin" "$bin/opencl_rust"
    echo "prebuild: opencl3 (Rust) cell -> /opt/cpu-opencl-compute/opencl_rust ($(stat -c %s "$rustbin") bytes, dynamically linked musl)"
}

compile_carpets() {
    local bin="$staging_root/opt/cpu-opencl-compute"; mkdir -p "$bin"
    local CL; CL="$(libpath libOpenCL)"

    echo "prebuild: cross-compile OpenCL carpets for $arch"
    # OpenCL carpets over rusticl/pocl; best-effort (libOpenCL may be absent on some arches -
    # la/rv in the Alpine edge package set as of 2026-07 have no mesa-rusticl or pocl package).
    if [[ -n "$CL" ]]; then
        # libOpenCL is present (gated above), so a compile failure here is a genuine breakage - let
        # set -e abort the prebuild rather than swallow it into a smaller-but-green on-target run.
        GCC -O2 "$CAR/opencl_c_full_api.c" -o "$bin/opencl_c" "$CL" -lm
        resolve_cxx
        cxx_object "$CAR/opencl_cpp_full_api.cpp" "$bin/opencl_cpp.o" -DCL_HPP_TARGET_OPENCL_VERSION=300
        cxx_link "$bin/opencl_cpp.o" "$bin/opencl_cpp" "$CL"
        rm -f "$bin/opencl_cpp.o"
        # opencl_rust (opencl3) + opencl_py (PyOpenCL) are built by finalize_dynamic_cells AFTER
        # integrate_pocl, so they ride whichever libOpenCL ends up present (rusticl here or pocl folded).
    else
        echo "prebuild: libOpenCL absent for $arch - opencl_c/opencl_cpp not built (no rusticl/pocl package)"
    fi
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) binary(ies) + run_all.sh"
}

# pocl (portable CPU OpenCL) provides OpenCL over LLVM on arches where mesa ships no rusticl (e.g.
# riscv64). This step is optional: if the env var POCL_PREBUILT points at a pocl staging tree for the
# matching arch/cpu (a directory with usr/lib/libOpenCL.so*, usr/lib/pocl, usr/share/pocl, etc/OpenCL
# and the LLVM/hwloc closure), its runtime is folded into the rootfs and the OpenCL cells are linked
# against pocl's libOpenCL. When POCL_PREBUILT is unset the step no-ops and OpenCL is served by
# rusticl where the arch has it (best-effort additive; the core gate does not depend on OpenCL).
integrate_pocl() {
    local pocl="${POCL_PREBUILT:-}" bin="$staging_root/opt/cpu-opencl-compute"
    [[ -n "$pocl" && -d "$pocl/usr" ]] || { echo "prebuild: no pocl staging for $arch (set POCL_PREBUILT to fold pocl) - OpenCL via rusticl only this run"; return 0; }
    cp -a "$pocl/usr/lib/libOpenCL.so"* "$staging_root/usr/lib/" 2>/dev/null || true
    cp -a "$pocl/usr/lib/libpocl.so"* "$staging_root/usr/lib/" 2>/dev/null || true
    cp -a "$pocl/usr/lib/pocl" "$staging_root/usr/lib/" 2>/dev/null || true
    cp -a "$pocl/usr/share/pocl" "$staging_root/usr/share/" 2>/dev/null || true
    for soname in libLLVM.so.20.1 libclang-cpp.so.20.1 libhwloc.so.15; do
        real="$(readlink -f "$pocl/usr/lib/$soname" 2>/dev/null || true)"
        [[ -n "$real" && -f "$real" ]] && cp -Lf "$real" "$staging_root/usr/lib/$soname" 2>/dev/null || true
    done
    cp -a "$pocl/etc/OpenCL" "$staging_root/etc/" 2>/dev/null || true
    # pocl's host-cpu driver bakes Intel SVML/IRC archive paths into HOST_LD_FLAGS. Provide
    # ABI-correct scalar-loop wrappers (svml_stub.c) and an empty libirc.a at those baked paths.
    local svml_dir="$staging_root/opt/intel/oneapi/compiler/latest/lib"
    mkdir -p "$svml_dir"
    if GCC -O2 -fPIC -ffp-contract=off -fno-math-errno -c "$app_dir/programs/svml_stub.c" -o "$svml_dir/svml_stub.o" 2>/dev/null; then
        ar rcs "$svml_dir/libsvml.a" "$svml_dir/svml_stub.o"; rm -f "$svml_dir/svml_stub.o"
        echo "prebuild: built libsvml.a ($(nm "$svml_dir/libsvml.a" 2>/dev/null | grep -c __svml_) __svml wrappers) for $arch"
    else
        printf '!<arch>\n' > "$svml_dir/libsvml.a"
    fi
    printf '!<arch>\n' > "$svml_dir/libirc.a"
    local pcl; pcl="$(ls "$staging_root"/usr/lib/libOpenCL.so.2* 2>/dev/null | head -1)"
    [[ -n "$pcl" ]] || return 0
    # pocl's libOpenCL IS present (gated above), so the OpenCL cells MUST compile+link against it: a
    # failure is a genuine breakage, not an honest arch gap (no `|| true` swallow).
    GCC -O2 "$CAR/opencl_c_full_api.c" -o "$bin/opencl_c" "$pcl" -lm -Wl,--allow-shlib-undefined
    [[ -x "$bin/opencl_c" ]] || { echo "prebuild: opencl_c failed to link against pocl on $arch (genuine breakage)" >&2; exit 4; }
    resolve_cxx
    cxx_object "$CAR/opencl_cpp_full_api.cpp" "$bin/opencl_cpp.o" -DCL_HPP_TARGET_OPENCL_VERSION=300
    cxx_link "$bin/opencl_cpp.o" "$bin/opencl_cpp" "$pcl" -Wl,--allow-shlib-undefined
    rm -f "$bin/opencl_cpp.o"
    [[ -x "$bin/opencl_cpp" ]] || { echo "prebuild: opencl_cpp failed to link against pocl on $arch (genuine breakage)" >&2; exit 4; }
    echo "prebuild: pocl OpenCL folded for $arch (opencl_c + opencl_cpp linked)"
}

# clvk (clvk_c) cell: OpenCL-over-Vulkan through the clvk ICD on Mesa lavapipe. clvk is built with
# CLVK_COMPILER_AVAILABLE=OFF (no clspv/LLVM online compiler - a small ~3 MiB Vulkan-only runtime that
# links libvulkan + libstdc++ + SPIRV-Tools, NOT LLVM), so it cannot compile OpenCL C at runtime.
# Instead the kernels are compiled to a clvk-native SPIR-V executable binary AT BUILD TIME on the host
# (host clspv + host clvk's clGetProgramInfo(CL_PROGRAM_BINARIES)) and shipped in the image; the cell
# loads it via clCreateProgramWithBinary. This keeps the on-target dependency to just clvk + lavapipe,
# with no LLVM in the guest.
#
# Provisioned only when CLVK_PREBUILT points at a per-arch clvk staging tree
#   $CLVK_PREBUILT/<arch>/usr/lib/libOpenCL.so*   (the no-compiler musl clvk for this arch)
#   $CLVK_PREBUILT/clvk_c_kernels.clvkbin         (the host-precompiled SPIR-V executable binary)
# and only where the target arch's musl clvk was cross-built (x86_64 as of this delivery). clvk's
# libOpenCL is staged in a DEDICATED dir (/opt/cpu-opencl-compute/clvk/lib) so it never collides with
# the pocl/rusticl libOpenCL the other cells link; the clvk_c wrapper points LD_LIBRARY_PATH there and
# selects the lavapipe Vulkan ICD. Vulkan (libvulkan.so.1 + lavapipe ICD) comes from the mesa closure
# already provisioned for rusticl.
integrate_clvk() {
    local clvk="${CLVK_PREBUILT:-}" bin="$staging_root/opt/cpu-opencl-compute"
    [[ -n "$clvk" ]] || { echo "prebuild: no clvk staging (set CLVK_PREBUILT to add the clvk_c cell) - clvk_c not wired this run"; return 0; }
    local clvk_lib_src="$clvk/$arch/usr/lib"
    local clvk_so; clvk_so="$(ls "$clvk_lib_src"/libOpenCL.so.* 2>/dev/null | head -1)"
    if [[ -z "$clvk_so" ]]; then
        echo "prebuild: no no-compiler musl clvk libOpenCL for $arch under $clvk_lib_src - clvk_c not wired (arch not cross-built)"
        return 0
    fi
    local kernbin="$clvk/clvk_c_kernels.clvkbin"
    [[ -f "$kernbin" ]] || { echo "prebuild: clvk kernel binary $kernbin missing - clvk_c not wired" >&2; return 0; }
    # A lavapipe Vulkan ICD must be present (clvk is a Vulkan client). It rides the mesa closure that
    # mesa-rusticl pulled in; if absent this arch cannot run clvk, so skip honestly.
    if ! ls "$staging_root"/usr/share/vulkan/icd.d/lvp_icd*.json >/dev/null 2>&1 && \
       ! ls "$staging_root"/usr/lib/libvulkan.so.* >/dev/null 2>&1; then
        echo "prebuild: no lavapipe/libvulkan in staging for $arch - clvk_c not wired (no Vulkan runtime)"
        return 0
    fi
    # Dedicated dir for clvk's libOpenCL so it does not collide with pocl/rusticl's libOpenCL.
    local clvk_dst="$bin/clvk/lib"; mkdir -p "$clvk_dst"
    cp -a "$clvk_lib_src"/libOpenCL.so* "$clvk_dst/"
    cp "$kernbin" "$bin/clvk_c_kernels.clvkbin"
    # Build-only bare .so symlink for the linker (Alpine/clvk ship only libOpenCL.so.<n>).
    local linkdir="$staging_root/opt/clvk-link"; mkdir -p "$linkdir"
    ln -sf "$clvk_dst/$(basename "$clvk_so")" "$linkdir/libOpenCL.so"
    echo "prebuild: cross-compile clvk_c cell for $arch (links clvk no-compiler libOpenCL)"
    GCC -O2 "$CAR/clvk_c/clvk_c_full_api.c" -o "$bin/clvk_c.elf" -L"$linkdir" -lOpenCL -lm
    [[ -x "$bin/clvk_c.elf" ]] || { echo "prebuild: clvk_c failed to cross-compile for $arch (genuine breakage)" >&2; exit 6; }
    # Wrapper: run the clvk_c ELF with clvk's libOpenCL first on the search path, the lavapipe Vulkan
    # ICD selected, the SPIR-V capability check relaxed (host-precompiled binary vs guest lavapipe),
    # and the precompiled kernel binary located.
    cat > "$bin/clvk_c" <<'CLVKW'
#!/bin/sh
BIN=/opt/cpu-opencl-compute
export LD_LIBRARY_PATH="$BIN/clvk/lib:/usr/lib"
LVP="$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)"
[ -n "$LVP" ] && export VK_ICD_FILENAMES="$LVP"
export CLVK_SKIP_SPIRV_CAPABILITY_CHECK=1
export CLVK_C_BINARY="$BIN/clvk_c_kernels.clvkbin"
export LP_NUM_THREADS=1
exec "$BIN/clvk_c.elf" "$@"
CLVKW
    chmod +x "$bin/clvk_c"
    echo "prebuild: clvk_c cell -> /opt/cpu-opencl-compute/clvk_c (clvk no-compiler over lavapipe, precompiled SPIR-V)"
}

# PyOpenCL (opencl_py) cell: stage the source + a wrapper. python3 + numpy + the py3-opencl native
# _cl extension come from apk (site-packages ride populate_overlay's cp -a of /usr/lib/.). Wired only
# where the _cl extension actually landed - so the cell rides the same runtime as the C/C++/Rust cells.
provision_python() {
    local bin="$staging_root/opt/cpu-opencl-compute"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 not provisioned - PyOpenCL cell not wired" >&2; return 0; }
    ls "$staging_root"/usr/lib/python3*/site-packages/pyopencl/_cl*.so >/dev/null 2>&1 \
        || { echo "prebuild: py3-opencl _cl extension absent for $arch - PyOpenCL cell not wired"; return 0; }
    cp "$CAR/opencl_py_full_api.py" "$bin/opencl_py.py"
    cat > "$bin/opencl_py" <<'PYW'
#!/bin/sh
export POCL_DEVICES=basic
export RUSTICL_ENABLE=llvmpipe
export OCL_ICD_VENDORS=/etc/OpenCL/vendors
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-opencl-compute/opencl_py.py "$@"
PYW
    chmod +x "$bin/opencl_py"
    echo "prebuild: PyOpenCL carpet -> /opt/cpu-opencl-compute/opencl_py (python3 + numpy + py3-opencl _cl extension)"
}

# opencl_rust (opencl3) + opencl_py (PyOpenCL) ride whichever libOpenCL is present AFTER both the
# rusticl (compile_carpets) and pocl (integrate_pocl) provisioning steps have run, so they build on
# both rusticl arches (x64/aa) and pocl arches (rv/la with POCL_PREBUILT).
finalize_dynamic_cells() {
    local bin="$staging_root/opt/cpu-opencl-compute"
    local CL; CL="$(libpath libOpenCL)"
    [[ -n "$CL" ]] || { echo "prebuild: no libOpenCL for $arch - opencl_rust/opencl_py not wired (ride the C cells' runtime)"; return 0; }
    compile_rust "$bin"
    provision_python
}

populate_overlay() {
    local bin="$staging_root/opt/cpu-opencl-compute"
    # Capability manifest: list exactly the cells provisioned on this arch. Every cell build hard-fails
    # (compile_carpets / integrate_pocl / compile_rust exit on error), so a binary that is present is one
    # that genuinely built - the manifest cannot silently under-count a cell that should have built.
    # run_all.sh gates on this exact set (fail==0 && total==EXPECTED==pass, EXPECTED>=2 floor).
    : > "$bin/expected_cells"
    for c in opencl_c opencl_cpp opencl_rust opencl_py clvk_c; do [[ -x "$bin/$c" ]] && echo "$c" >> "$bin/expected_cells"; done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$bin/expected_cells")"
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin" "$overlay_dir/etc"
    cp -a "$staging_root/etc/OpenCL" "$overlay_dir/etc/" 2>/dev/null || true
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/pocl" "$overlay_dir/usr/share/" 2>/dev/null || true
    # python3 interpreter for the PyOpenCL cell (its site-packages + _cl extension ride /usr/lib/. above)
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-opencl-compute" "$overlay_dir/opt/"
    cp -a "$staging_root/opt/intel" "$overlay_dir/opt/" 2>/dev/null || true
    ln -sf /opt/cpu-opencl-compute/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
integrate_pocl
integrate_clvk
finalize_dynamic_cells
populate_overlay
