#!/usr/bin/env bash
# prebuild.sh - provision the software GPU compute runtime (Mesa lavapipe / llvmpipe, the CPU software
# Vulkan driver) and build the wgpu (WebGPU) Rust compute carpet into the per-arch Alpine rootfs.
#
# On-target model (identical driver stack to the merged gpu-vulkan app): extract the base Alpine
# rootfs to a staging tree, `apk add` mesa-vulkan-swrast (lavapipe) + the Vulkan loader via
# qemu-user-static (apk resolves every package for the TARGET arch on an x86 build host - no drifting
# URLs, no cache-miss-exit), then cross-compile the wgpu Rust carpet to <arch>-unknown-linux-musl. The
# wgpu crate carries its own wgpu-core/naga and reaches the GPU through the ash Vulkan backend, which
# dlopens libvulkan.so.1 at runtime; that loader plus lavapipe are the exact software Vulkan stack the
# gpu-vulkan app already runs on-target on all four arches. Finally copy the /usr/lib closure, the
# lavapipe ICD metadata and the carpet binary + runner into the overlay. Inputs are the base rootfs and
# the Alpine edge apk repos only.
#
# lavapipe runs the Vulkan compute queue on llvmpipe (LLVM CPU JIT), so no host GPU is required. Alpine
# edge builds mesa-vulkan-swrast for all four target arches, so the wgpu Rust carpet runs on-target on
# every arch. The C / C++ / Python wgpu bindings drive wgpu-native (a prebuilt cdylib gfx-rs ships only
# as linux-x86_64 / linux-aarch64 glibc, no musl / riscv64 / loongarch64) and wgpu-py (conda-forge,
# glibc x86_64 / aarch64 only), so they are host-reference; the on-target gate is the Rust cell, which
# builds its own wgpu-core against musl and needs only the Vulkan loader + lavapipe at runtime.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
RSDIR="$app_dir/rssrc/wgpu-carpet"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     rust_target="aarch64-unknown-linux-musl";     musl_cc="aarch64-linux-musl-gcc" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     rust_target="riscv64gc-unknown-linux-musl";   musl_cc="riscv64-linux-musl-gcc" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      rust_target="x86_64-unknown-linux-musl";      musl_cc="x86_64-linux-musl-gcc" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; rust_target="loongarch64-unknown-linux-musl"; musl_cc="loongarch64-linux-musl-gcc" ;;
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

# mesa software Vulkan (lavapipe) + LLVM + the Vulkan loader, all musl for the target arch. mesa-dev is
# intentionally NOT installed (it pulls the ~200MB clang-libs closure the runtime does not need).
# Alpine builds mesa-vulkan-swrast for every arch.
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers zlib)

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

# Cross-compile the wgpu Rust carpet to <arch>-unknown-linux-musl. Notes:
#  - dynamic musl (`-C target-feature=-crt-static`) is REQUIRED. The musl default is a fully static
#    binary whose dlopen is a NULL stub, so ash's runtime dlopen("libvulkan.so.1") returns nothing and
#    wgpu reports "no adapter". A dynamic-musl PIE links the real musl loader, so dlopen resolves the
#    staged Vulkan loader -> lavapipe.
#  - the toolchain is pinned to the workspace nightly (rust-toolchain.toml selects a no_std kernel
#    channel; the musl std for the host tools lives in that same nightly, selected explicitly here).
#  - cargo inherits every ancestor .cargo/config.toml, so a build host whose global config
#    source-replaces crates.io with an unreachable mirror would fail. Build from a scratch copy under a
#    fresh CARGO_HOME so cargo uses only the default crates.io sparse index (immune to the host mirror,
#    reproducible on a clean host). --locked pins the committed Cargo.lock.
RUST_CHANNEL="${GPU_WGPU_RUST_CHANNEL:-nightly-2026-05-28-x86_64-unknown-linux-gnu}"
build_rust_carpet() {
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required to build the wgpu Rust carpet" >&2; exit 5; }
    command -v "$musl_cc" >/dev/null 2>&1 || { echo "prebuild: $musl_cc required on PATH to cross-link the musl carpet for $arch" >&2; exit 5; }
    local bin="$staging_root/opt/gpu-wgpu"; mkdir -p "$bin"
    local rsbuild rsout rshome
    rsbuild="$(mktemp -d)"; rsout="$(mktemp -d)"; rshome="$(mktemp -d)"
    cp -a "$RSDIR/." "$rsbuild/"
    local link_var="CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER"
    local cc_var="CC_$(echo "$rust_target" | tr '-' '_')"
    echo "prebuild: cross-build wgpu Rust carpet -> $rust_target (dynamic musl, lavapipe at runtime)"
    if ( cd "$rsbuild" && env \
            CARGO_HOME="$rshome" CARGO_TARGET_DIR="$rsout" \
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
            "$cc_var=$musl_cc" "$link_var=$musl_cc" \
            RUSTFLAGS="-C target-feature=-crt-static" \
            cargo "+$RUST_CHANNEL" build --release --locked --target "$rust_target" ) \
       && [[ -f "$rsout/$rust_target/release/wgpu-carpet" ]]; then
        install -Dm0755 "$rsout/$rust_target/release/wgpu-carpet" "$bin/wgpu_rust"
        echo "prebuild: staged wgpu_rust for $rust_target (dynamic musl PIE, dlopens libvulkan.so.1 -> lavapipe)"
    else
        echo "prebuild: wgpu Rust carpet failed to build for $rust_target" >&2
        rm -rf "$rsbuild" "$rsout" "$rshome"
        exit 5
    fi
    rm -rf "$rsbuild" "$rsout" "$rshome"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    # the whole provisioned /usr/lib closure (mesa lavapipe + LLVM + Vulkan loader) and ICD metadata
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/gpu-wgpu" "$overlay_dir/opt/"
    ln -sf /opt/gpu-wgpu/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
build_rust_carpet
populate_overlay
