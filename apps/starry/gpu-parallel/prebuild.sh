#!/usr/bin/env bash
# prebuild.sh - provision the software GPU compute runtime (Mesa lavapipe / llvmpipe for Vulkan,
# rusticl / pocl for OpenCL, both over LLVM CPU JIT) and the compiled parallel-compute carpet
# binaries into the per-arch Alpine rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-vulkan-swrast
# (lavapipe), the Vulkan loader, glslang/shaderc (GLSL -> SPIR-V) and the build toolchain INTO it via
# qemu-user-static (apk resolves every package for the TARGET arch on an x86 build host - no drifting
# URLs, no cache-miss-exit), then best-effort `apk add` mesa-rusticl + opencl-icd-loader for the
# OpenCL carpet. Cross-compile the Vulkan and OpenCL carpet sources against the provisioned musl
# headers/libraries with the target gcc under qemu-user, compile the GLSL compute shaders to SPIR-V,
# then copy the shared-library closure, the lavapipe ICD metadata and the carpet binaries + runner
# into the overlay. Inputs are the base rootfs and the Alpine edge apk repos only.
#
# All backends are CPU software: lavapipe runs the Vulkan compute queue on llvmpipe (LLVM CPU JIT),
# rusticl/pocl run OpenCL on the same CPU JIT, so no host GPU is required. Alpine edge builds
# mesa-vulkan-swrast for all four target arches, so the Vulkan carpet runs on-target on every arch;
# mesa-rusticl is a Rust component Alpine does not build for every arch, so the OpenCL carpet is
# additive (best-effort). If POCL_PREBUILT points at a matching-arch pocl staging tree, pocl's
# libOpenCL is folded in and the OpenCL carpet is linked against it instead.
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
    aarch64)     qemu_runner="qemu-aarch64-static";     apk_arch="aarch64" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     apk_arch="riscv64" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      apk_arch="x86_64" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; apk_arch="loongarch64" ;;
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
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers opencl-headers
          build-base glslang shaderc
          gmp mpfr4 mpc1 isl26 zlib)
# OpenCL over llvmpipe (rusticl, a Rust mesa component Alpine does not build for every arch) + the
# ICD loader. Best-effort: on arches without it the OpenCL carpet is skipped.
GPU_PKGS_OPT=(mesa-rusticl opencl-icd-loader)

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
    # best-effort: rusticl (OpenCL) is not built for every Alpine arch
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${GPU_PKGS_OPT[@]}"; then
        echo "prebuild: OpenCL rusticl provisioned for $arch"
    else
        echo "prebuild: OpenCL rusticl unavailable for $arch (upstream Alpine arch gap) - Vulkan carpet only"
    fi
}

# Cross-compile every carpet with the staging's target gcc/g++ under qemu-<arch>-static. --sysroot
# points every built-in header/library path at the staging tree (qemu-user does not redirect the
# compiler's own open() calls, so without it the musl headers mix with the host glibc /usr/include).
# The staged Alpine toolchain (gcc 15 + binutils 2.45) is used for every arch including the native one:
# Alpine's current libvulkan.so.1 carries a .relr.dyn (RELR) section that an older host musl-cross ld
# cannot add symbols from, and the staged binutils 2.45 links it correctly. qemu-user runs the gcc
# driver and its cc1/as/ld children under emulation, so this works with or without a binfmt entry for
# the native arch.
GCC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/gcc" --sysroot="$staging_root" "$@"; }
GPP() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/g++" --sysroot="$staging_root" "$@"; }
GLSLC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/glslc" "$@"; }

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

compile_carpets() {
    local bin="$staging_root/opt/gpu-parallel"; mkdir -p "$bin/shaders"
    local VK; VK="$(libpath libvulkan)"
    local CL; CL="$(libpath libOpenCL)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan not provisioned" >&2; exit 4; }

    # Vulkan compute shaders -> SPIR-V, kept next to the vk_parallel binary. vk_parallel loads
    # shaders/{vadd,partial_reduce,chain,atomic_sum}.spv (all 256-wide local size).
    for comp in "$CAR"/vk_parallel/shaders/*.comp; do
        [[ -f "$comp" ]] || continue
        GLSLC -O "$comp" -o "$bin/shaders/$(basename "${comp%.comp}").spv"
    done

    echo "prebuild: cross-compile parallel carpets for $arch (lavapipe Vulkan + rusticl/pocl OpenCL)"
    # Vulkan (hard-required core: the lavapipe parallel-compute path is the gate on every arch)
    GCC -O2 "$CAR/vk_parallel/vk_parallel_full_api.c" -o "$bin/vk_parallel" "$VK" -lm
    [[ -x "$bin/vk_parallel" ]] || { echo "prebuild: vk_parallel failed to compile" >&2; exit 4; }
    # OpenCL (additive: only where the arch provisioned a libOpenCL via rusticl)
    if [[ -n "$CL" ]]; then
        GCC -O2 "$CAR/cl_parallel/cl_parallel_full_api.c" -o "$bin/cl_parallel" "$CL" -lm || true
        [[ -x "$bin/cl_parallel" ]] && echo "prebuild: cl_parallel linked against $(basename "$CL")" \
            || echo "prebuild: cl_parallel did not link on $arch (OpenCL loader present but link failed) - Vulkan only"
    else
        echo "prebuild: no libOpenCL for $arch (no rusticl/pocl) - OpenCL parallel carpet skipped"
    fi
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) parallel carpet binary(ies) + run_all.sh"
}

# Optional: fold a matching-arch pocl staging tree (POCL_PREBUILT) into the rootfs as the OpenCL
# runtime and re-link the OpenCL carpet against pocl's libOpenCL. No-ops when POCL_PREBUILT is unset
# (OpenCL then comes from rusticl where the arch has it). Mirrors the sibling gpu-compute prebuild.
integrate_pocl() {
    local pocl="${POCL_PREBUILT:-}" bin="$staging_root/opt/gpu-parallel"
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
    local pcl; pcl="$(ls "$staging_root"/usr/lib/libOpenCL.so.2* 2>/dev/null | head -1)"
    [[ -n "$pcl" ]] || return 0
    GCC -O2 "$CAR/cl_parallel/cl_parallel_full_api.c" -o "$bin/cl_parallel" "$pcl" -lm -Wl,--allow-shlib-undefined \
        && echo "prebuild: pocl OpenCL folded for $arch (cl_parallel linked against pocl)" || true
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin" "$overlay_dir/etc"
    cp -a "$staging_root/etc/OpenCL" "$overlay_dir/etc/" 2>/dev/null || true
    # the whole provisioned /usr/lib closure (mesa lavapipe + LLVM + loaders) and ICD metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/usr/share/pocl"   "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-parallel" "$overlay_dir/opt/"
    ln -sf /opt/gpu-parallel/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
integrate_pocl
populate_overlay
