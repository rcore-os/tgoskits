#!/usr/bin/env bash
# prebuild.sh - provision the software OpenGL ES compute runtime (Mesa llvmpipe + the EGL
# surfaceless platform) and the compiled GLES compute carpet binaries into the per-arch Alpine
# rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-gles (the
# GLES 3.1 client library over llvmpipe), mesa-egl (EGL, including the EGL_MESA_platform_surfaceless
# path used to create a headless context), mesa-dri-gallium (the llvmpipe CPU driver) and the build
# toolchain INTO it via qemu-user-static (apk resolves every package for the TARGET arch on an x86
# build host - no drifting URLs, no cache-miss-exit), cross-compile the GLES C and C++ carpet sources
# against the provisioned musl libraries with the target gcc under qemu-user (the arch-independent
# EGL/GLES2/GLES3/KHR client headers are vendored under programs/headers, since Alpine carries them
# only in mesa-dev), then copy the shared-library closure and the carpet binaries + runner into the
# overlay. Inputs are the base rootfs and the Alpine edge apk repos only.
#
# All backends are CPU software: llvmpipe runs the GLES 3.1 compute pipeline on the LLVM CPU JIT and
# EGL creates the context on EGL_PLATFORM=surfaceless, so no host GPU or display server is required.
# Alpine edge builds mesa-gles / mesa-egl / mesa-dri-gallium for all four target arches
# (x86_64 / aarch64 / riscv64 / loongarch64), so the C/C++ carpets run on-target on every arch.
#
# The Python (moderngl) and Rust (glow + khronos-egl) cells under programs/carpets are exercised in
# the host reference layer only: their language runtimes (CPython + moderngl/numpy, rustc/cargo +
# glow) are not part of the musl on-target provisioning, so they do not run on StarryOS. Their raw
# GLES-over-EGL equivalents (gles_c / gles_cpp) are what run on-target on every arch.
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
# arch. mesa-dev is intentionally NOT installed (it pulls the ~200MB clang-libs closure the runtime
# does not need; the EGL/GLES/KHR client headers are vendored under programs/headers instead). Alpine
# builds mesa-gles / mesa-egl / mesa-dri-gallium for every arch.
GPU_PKGS=(musl mesa-gles mesa-egl mesa-dri-gallium
          build-base
          gmp mpfr4 mpc1 isl26 zlib)

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
}

# cross-compile one carpet with the staging's target gcc under qemu-user. --sysroot points every
# built-in header/library path at the staging tree (qemu-user does not redirect the compiler's own
# open() calls, so without it the musl C++ headers mix with the host glibc /usr/include).
GCC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/gcc" --sysroot="$staging_root" "$@"; }
GPP() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/g++" --sysroot="$staging_root" "$@"; }

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

compile_carpets() {
    local bin="$staging_root/opt/gpu-gles"; mkdir -p "$bin"
    local hdr="$app_dir/programs/headers"
    local EGL; EGL="$(libpath libEGL)"
    local GLESv2; GLESv2="$(libpath libGLESv2)"
    [[ -n "$EGL" ]] || { echo "prebuild: libEGL not provisioned" >&2; exit 4; }
    [[ -n "$GLESv2" ]] || { echo "prebuild: libGLESv2 not provisioned" >&2; exit 4; }

    echo "prebuild: cross-compile GLES carpets for $arch (llvmpipe compute, EGL surfaceless)"
    # GLES 3.1 compute over EGL-surfaceless + GLESv2. The vendored EGL/GLES2/GLES3/KHR client headers
    # (-I"$hdr") declare the API; the target-arch libEGL/libGLESv2 sonames are linked directly (Alpine
    # ships no bare -lEGL/-lGLESv2 .so symlink under this staging).
    GCC -O2 -I"$hdr" "$CAR/gles_c/gles_c_full_api.c" -o "$bin/gles_c" "$EGL" "$GLESv2" -lm
    GPP -O2 -std=c++17 -I"$hdr" "$CAR/gles_cpp/gles_cpp_full_api.cpp" -o "$bin/gles_cpp" "$EGL" "$GLESv2" -lm
    for f in gles_c gles_cpp; do
        [[ -x "$bin/$f" ]] || { echo "prebuild: carpet $f failed to compile" >&2; exit 4; }
    done
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) GLES carpet binary(ies) + run_all.sh"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa GLES + EGL + llvmpipe DRI + LLVM) and vendor metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/glvnd" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-gles" "$overlay_dir/opt/"
    ln -sf /opt/gpu-gles/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
