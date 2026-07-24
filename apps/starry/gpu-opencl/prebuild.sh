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

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     apk_arch="aarch64";     gcc_triple="aarch64-alpine-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     apk_arch="riscv64";     gcc_triple="riscv64-alpine-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      apk_arch="x86_64";      gcc_triple="x86_64-alpine-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; apk_arch="loongarch64"; gcc_triple="loongarch64-alpine-linux-musl" ;;
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

# build toolchain + OpenCL headers, all musl for the target arch.
GPU_PKGS=(musl build-base opencl-headers gmp mpfr4 mpc1 isl26 zlib)
# Optional: mesa-rusticl (OpenCL over llvmpipe) + ICD loader. Not available on every Alpine arch
# (absent on riscv64 as of Alpine edge 2026-07); absent arches are served by pocl via POCL_PREBUILT.
GPU_PKGS_OPT=(mesa-rusticl opencl-icd-loader)

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
}

# Compiler selection.
#
# Foreign target (aa/rv/la on an x86_64 build host): the Alpine gcc/g++ in the staging tree are
# foreign ELFs. The kernel's binfmt_misc F-flag routes both the compiler driver and every child it
# execve()s (cc1, cc1plus, as, ld) through qemu-<arch>-static automatically, and -L points qemu at
# the staging tree for the interpreter, so the whole toolchain runs correctly under emulation.
#
# Native target (x86_64 on an x86_64 build host): there is no qemu-x86_64 binfmt entry, so gcc is
# launched by name under qemu-x86_64-static. The -L flag only helps qemu load gcc itself; when gcc's
# driver execve()s cc1 the kernel runs that native x86_64 ELF WITHOUT qemu, and cc1's musl loader
# (/lib/ld-musl-x86_64.so.1) is not present on the glibc build host -> "cannot execute cc1:
# posix_spawn: No such file", failing the build before the guest ever boots. So for the native arch
# use a host cross toolchain (x86_64-linux-musl-gcc/g++, itself a native binary whose own cc1
# resolves natively) with --sysroot pointing at the Alpine staging tree; it emits the same
# /lib/ld-musl-x86_64.so.1 binaries Alpine expects. --sysroot points every built-in header/library
# path at the staging tree. Alpine ships no -lOpenCL .so symlink, so link the full soname.
host_arch="$(uname -m)"
if [[ "$arch" == "$host_arch" ]]; then
    host_cc="$(command -v "${host_arch}-linux-musl-gcc" || true)"
    host_cxx="$(command -v "${host_arch}-linux-musl-g++" || true)"
    [[ -x "$host_cc" && -x "$host_cxx" ]] || { echo "prebuild: native arch $arch needs a host ${host_arch}-linux-musl cross toolchain on PATH (staged Alpine gcc's cc1 cannot run natively on the glibc host)" >&2; exit 1; }
    GCC() { "$host_cc" --sysroot="$staging_root" "$@"; }
    GPP() { "$host_cxx" --sysroot="$staging_root" "$@"; }
else
    GCC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
            "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/gcc" --sysroot="$staging_root" "$@"; }
    GPP() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
            "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/g++" --sysroot="$staging_root" "$@"; }
fi

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

compile_carpets() {
    local bin="$staging_root/opt/gpu-opencl"; mkdir -p "$bin"
    local CL; CL="$(libpath libOpenCL)"

    echo "prebuild: cross-compile OpenCL carpets for $arch"
    # OpenCL carpets over rusticl/pocl; best-effort (libOpenCL may be absent on some arches -
    # la/rv in the Alpine edge package set as of 2026-07 have no mesa-rusticl or pocl package).
    if [[ -n "$CL" ]]; then
        GCC -O2 "$CAR/opencl_c_full_api.c" -o "$bin/opencl_c" "$CL" -lm || true
        GPP -O2 -std=c++17 -DCL_HPP_TARGET_OPENCL_VERSION=300 "$CAR/opencl_cpp_full_api.cpp" -o "$bin/opencl_cpp" "$CL" || true
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
    local pocl="${POCL_PREBUILT:-}" bin="$staging_root/opt/gpu-opencl"
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
    GCC -O2 "$CAR/opencl_c_full_api.c" -o "$bin/opencl_c" "$pcl" -lm -Wl,--allow-shlib-undefined \
        && echo "prebuild: pocl OpenCL folded for $arch (opencl_c linked)" || true
    GPP -O2 -std=c++17 -DCL_HPP_TARGET_OPENCL_VERSION=300 "$CAR/opencl_cpp_full_api.cpp" -o "$bin/opencl_cpp" "$pcl" -Wl,--allow-shlib-undefined || true
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin" "$overlay_dir/etc"
    cp -a "$staging_root/etc/OpenCL" "$overlay_dir/etc/" 2>/dev/null || true
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/pocl" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-opencl" "$overlay_dir/opt/"
    cp -a "$staging_root/opt/intel" "$overlay_dir/opt/" 2>/dev/null || true
    ln -sf /opt/gpu-opencl/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
integrate_pocl
populate_overlay
