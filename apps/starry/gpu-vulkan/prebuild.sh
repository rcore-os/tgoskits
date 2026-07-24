#!/usr/bin/env bash
# prebuild.sh - provision the software Vulkan compute runtime (Mesa lavapipe / llvmpipe + the Vulkan
# loader) and the compiled Vulkan compute carpet binaries into the per-arch Alpine rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-vulkan-swrast
# (lavapipe, the CPU software Vulkan driver), vulkan-loader, glslang/shaderc (GLSL -> SPIR-V) and the
# build toolchain INTO it via qemu-user-static (apk resolves every package for the TARGET arch on an
# x86 build host - no drifting URLs, no cache-miss-exit), cross-compile the Vulkan C and C++ carpet
# sources against the provisioned musl headers/libraries with the target gcc under qemu-user, compile
# the GLSL compute shaders to SPIR-V, then copy the shared-library closure, the lavapipe ICD metadata
# and the carpet binaries + runner into the overlay. Inputs are the base rootfs and the Alpine edge
# apk repos only.
#
# All backends are CPU software: lavapipe runs the Vulkan compute queue on llvmpipe (LLVM CPU JIT),
# so no host GPU is required. Alpine edge builds mesa-vulkan-swrast for all four target arches
# (x86_64 / aarch64 / riscv64 / loongarch64), so the C/C++ carpets run on-target on every arch.
#
# The Rust (ash) and Python (pyvulkan / kompute) cells under programs/carpets are exercised in the
# host reference layer only: their language runtimes (rustc/cargo, CPython + pyvulkan/kompute) are
# not part of the musl on-target provisioning, so they do not run on StarryOS. kompute's prebuilt
# binaries are glibc x86_64 / aarch64 only (conda-forge builds no linux-riscv64 / linux-loongarch64
# kompute), so the kompute cell's runtime is host-side; its raw-Vulkan equivalent (vulkan_c /
# vulkan_cpp) is what runs on-target on every arch.
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
          gmp mpfr4 mpc1 isl26 zlib)

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

# cross-compile one carpet with the staging's target gcc under qemu-user. --sysroot points every
# built-in header/library path at the staging tree (qemu-user does not redirect the compiler's own
# open() calls, so without it the musl C++ headers mix with the host glibc /usr/include). Alpine
# ships no -lvulkan .so symlink, so link the full soname path.
GCC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/gcc" --sysroot="$staging_root" "$@"; }
GPP() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/g++" --sysroot="$staging_root" "$@"; }
GLSLC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/glslc" "$@"; }

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

compile_carpets() {
    local bin="$staging_root/opt/gpu-vulkan"; mkdir -p "$bin/shaders"
    local VK; VK="$(libpath libvulkan)"
    [[ -n "$VK" ]] || { echo "prebuild: libvulkan not provisioned" >&2; exit 4; }

    # Vulkan compute shaders -> SPIR-V, kept next to the binaries. vulkan_c loads shaders/vadd.spv +
    # shaders/mul.spv; vulkan_cpp reuses shaders/vadd.spv. Both dispatch (N+63)/64 groups.
    for comp in "$CAR"/vulkan_c/shaders/*.comp; do
        [[ -f "$comp" ]] || continue
        GLSLC -O "$comp" -o "$bin/shaders/$(basename "${comp%.comp}").spv"
    done

    echo "prebuild: cross-compile Vulkan carpets for $arch (lavapipe compute)"
    GCC -O2 "$CAR/vulkan_c/vulkan_c_full_api.c" -o "$bin/vulkan_c" "$VK" -lm
    GPP -O2 -std=c++17 "$CAR/vulkan_cpp/vulkan_cpp_full_api.cpp" -o "$bin/vulkan_cpp" "$VK"
    for f in vulkan_c vulkan_cpp; do
        [[ -x "$bin/$f" ]] || { echo "prebuild: carpet $f failed to compile" >&2; exit 4; }
    done
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) Vulkan carpet binary(ies) + run_all.sh"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa lavapipe + LLVM + the Vulkan loader) and ICD metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-vulkan" "$overlay_dir/opt/"
    ln -sf /opt/gpu-vulkan/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
