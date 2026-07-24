#!/usr/bin/env bash
# prebuild.sh - provision the software desktop-OpenGL compute runtime (Mesa llvmpipe over the gallium
# DRI path + libGL + libEGL) and the compiled OpenGL compute carpet binaries into the per-arch Alpine
# rootfs.
#
# Portable model: extract the base Alpine rootfs to a staging tree, `apk add` mesa-gl / mesa-egl /
# mesa-gles / mesa-dri-gallium (the llvmpipe CPU software GL/GLES stack) and the build toolchain INTO
# it via qemu-user-static (apk resolves every package for the TARGET arch on an x86 build host - no
# drifting URLs, no cache-miss-exit), cross-compile the OpenGL compute carpet sources against the
# provisioned musl headers/libraries with the target gcc under qemu-user, then copy the shared-library
# closure, the EGL/GL vendor metadata and the carpet binaries + runner into the overlay. The
# arch-independent GL/glcorearb.h + EGL + KHR headers are vendored under programs/headers (Alpine's
# mesa-dev is the only package carrying the desktop GL/glcorearb.h and it pulls a large clang closure,
# so the pared-down headers are shipped with the app instead). Inputs are the base rootfs and the
# Alpine edge apk repos only.
#
# All backends are CPU software: Mesa's llvmpipe runs the GL 4.3 compute pipeline (glDispatchCompute)
# on the LLVM CPU JIT, so no host GPU is required. Alpine edge builds mesa-gl / mesa-egl /
# mesa-dri-gallium for all four target arches (x86_64 / aarch64 / riscv64 / loongarch64), so the
# surfaceless-EGL desktop-GL carpet (opengl_c_egl) runs on-target on every arch.
#
# Alpine ships no mesa-osmesa package on any arch, so libOSMesa is absent on-target: the OSMesa
# carpets (opengl_c, opengl_cpp, opengl_py) and the moderngl / glow cells are exercised in the host
# reference layer only. opengl_c_egl reaches the identical GL 4.3 compute surface (compile/link/SSBO/
# dispatch/barrier/readback) through EGL-surfaceless instead of OSMesa, so the on-target gate covers
# the desktop-GL compute path on every arch. The OSMesa carpet builds are wired here best-effort for
# any arch that later gains mesa-osmesa.
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
# per-app image must be grown here first. The overlay carries the full mesa/llvmpipe closure plus its
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

# mesa software desktop-GL + GLES + EGL + the gallium DRI drivers (llvmpipe) + LLVM + build toolchain,
# all musl for the target arch. mesa-dev is intentionally NOT installed (it pulls the ~200MB clang-libs
# closure the runtime does not need; the GL/EGL/KHR headers are vendored under programs/headers).
# Alpine builds mesa-gl / mesa-egl / mesa-gles / mesa-dri-gallium for every arch.
GPU_PKGS=(musl mesa-gl mesa-egl mesa-gles mesa-dri-gallium
          build-base
          gmp mpfr4 mpc1 isl26 zlib)
# best-effort, its own transaction: OSMesa (off-screen desktop GL) for the opengl_c / opengl_cpp
# carpets. Alpine's mesa build ships no OSMesa package on any arch today, so this add is expected to
# no-op; kept separate so a missing OSMesa never fails the whole GPU_PKGS transaction.
GPU_PKGS_OSMESA=(mesa-osmesa)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add desktop-GL stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -f "$staging_root/usr/lib/libGL.so.1" || -n "$(ls "$staging_root/usr/lib/libGL.so"* 2>/dev/null)" ]] \
        || { echo "prebuild: mesa-gl (libGL) not provisioned" >&2; exit 3; }
    [[ -n "$(ls "$staging_root/usr/lib/libEGL.so"* 2>/dev/null)" ]] \
        || { echo "prebuild: mesa-egl (libEGL) not provisioned" >&2; exit 3; }
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" add "${GPU_PKGS_OSMESA[@]}" 2>/dev/null; then
        echo "prebuild: OSMesa provisioned for $arch (desktop-GL opengl_c/opengl_cpp buildable on-target)"
    else
        echo "prebuild: OSMesa unavailable for $arch (Alpine ships no mesa-osmesa) - opengl_c/opengl_cpp host-only"
    fi
}

# cross-compile one carpet with the staging's target gcc under qemu-user. --sysroot points every
# built-in header/library path at the staging tree (qemu-user does not redirect the compiler's own
# open() calls, so without it the musl C++ headers mix with the host glibc /usr/include). Alpine
# ships the -lGL/-lEGL .so symlinks via mesa-gl/mesa-egl, but link the resolved soname path to be safe.
GCC() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/gcc" --sysroot="$staging_root" "$@"; }
GPP() { QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/g++" --sysroot="$staging_root" "$@"; }

libpath() { ls "$staging_root/usr/lib/$1".so* 2>/dev/null | head -1 || true; }

compile_carpets() {
    local bin="$staging_root/opt/gpu-opengl"; mkdir -p "$bin"
    local hdr="$app_dir/programs/headers"
    local EGL; EGL="$(libpath libEGL)"
    local GL;  GL="$(libpath libGL)"
    local OSM; OSM="$(libpath libOSMesa)"
    [[ -n "$EGL" && -n "$GL" ]] || { echo "prebuild: libEGL/libGL not provisioned" >&2; exit 4; }

    echo "prebuild: cross-compile OpenGL compute carpets for $arch (llvmpipe desktop-GL 4.3)"
    # desktop GL via EGL-surfaceless: 1.x symbols from libGL, GL 4.3 compute entry points resolved at
    # runtime via eglGetProcAddress. This is the on-target gate carpet (buildable+runnable on every
    # arch that has mesa-gl + mesa-egl, which Alpine builds for all four).
    GCC -O2 -I"$hdr" "$CAR/opengl_c_egl/opengl_c_egl_full_api.c" -o "$bin/opengl_c_egl" "$EGL" "$GL" -lm

    # OSMesa desktop-GL off-screen carpets. Alpine ships no OSMesa on any arch today, so libOSMesa is
    # normally absent and these are skipped; wired best-effort for arches that later gain mesa-osmesa.
    if [[ -n "$OSM" ]]; then
        GCC -O2 -I"$hdr" "$CAR/opengl_c/opengl_c_full_api.c" -o "$bin/opengl_c" "$OSM" -lm || true
        GPP -O2 -std=c++17 -I"$hdr" "$CAR/opengl_cpp/opengl_cpp_full_api.cpp" -o "$bin/opengl_cpp" "$OSM" -lm || true
    else
        echo "prebuild: libOSMesa absent for $arch - opengl_c/opengl_cpp (OSMesa desktop GL) host-only"
    fi

    [[ -x "$bin/opengl_c_egl" ]] || { echo "prebuild: opengl_c_egl failed to compile" >&2; exit 4; }
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
    echo "prebuild: compiled $(find "$bin" -maxdepth 1 -type f -perm -u+x ! -name '*.sh' | wc -l) OpenGL carpet binary(ies) + run_all.sh"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa llvmpipe + LLVM + libGL/libEGL + gallium DRI)
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/glvnd" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-opengl" "$overlay_dir/opt/"
    ln -sf /opt/gpu-opengl/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
compile_carpets
populate_overlay
