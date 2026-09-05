#!/usr/bin/env bash
# prebuild.sh - provision the software WebGPU render runtime (Mesa lavapipe for the wgpu Vulkan backend +
# Mesa llvmpipe/EGL for the wgpu GL backend) and build the wgpu (WebGPU) render carpet cells into the
# per-arch Alpine rootfs. Render counterpart of cpu-wgpu-compute (#1576): instead of compute pipelines
# it renders offscreen into an RGBA8Unorm texture through real render pipelines, copies the texture to a
# MAP_READ buffer and checks pixels back against a closed-form reference. Every cell is run under BOTH
# software backends (WGPU_BACKEND=vulkan / gl) by run_all.sh.
#
# On-target model (identical driver stack to cpu-wgpu-compute): extract the base Alpine rootfs, `apk add`
# mesa-vulkan-swrast (lavapipe) + vulkan-loader + mesa-gl/mesa-egl/mesa-dri-gallium (llvmpipe GL) via
# qemu-user-static, then per cell:
#   - wgpu_render_rust: cross-compile the wgpu crate carpet to <arch>-unknown-linux-musl (dynamic musl;
#     wgpu carries its own wgpu-core/naga, dlopens libvulkan.so.1 / libEGL at runtime). On-target gate.
#   - wgpu_render_c / wgpu_render_cpp: dynamic-musl executables linking libwgpu_native.so, which is built
#     from gfx-rs wgpu-native source (v22, matching the Rust cell's wgpu crate) for musl - gfx-rs ships
#     only glibc x86_64/aarch64 prebuilts, so it is built here from source.
#   - wgpu_render_py: the pure-python wgpu-py sdist + WGPU_LIB_PATH pointing at the built musl
#     libwgpu_native (same recipe as the compute cell).
# A capability manifest lists exactly the cells provisioned on this arch; run_all.sh gates on that set
# under both backends (fail==0 && total==2*cells==pass, >=1 cell floor).
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
RSDIR="$CAR/wgpu_render_rust"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static";     rust_target="aarch64-unknown-linux-musl";     triple="aarch64-linux-musl" ;;
    riscv64)     qemu_runner="qemu-riscv64-static";     rust_target="riscv64gc-unknown-linux-musl";   triple="riscv64-linux-musl" ;;
    x86_64)      qemu_runner="qemu-x86_64-static";      rust_target="x86_64-unknown-linux-musl";      triple="x86_64-linux-musl" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static"; rust_target="loongarch64-unknown-linux-musl"; triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# Resolve the HOST musl-cross C / C++ compiler for the target arch. The target gcc/g++ cannot run
# under qemu-user (cc1/cc1plus posix_spawn fails), so cells are cross-compiled on the host: standard
# ${triple}-gcc on PATH, then the conventional /opt/${triple}-cross install prefix (musl.cc layout),
# then `zig cc/c++ -target ${triple}` as a portable single-toolchain fallback. `musl_cc`/`musl_cxx`
# are command arrays so the zig fallback carries its -target argument.
ZIG_BIN="$(command -v zig 2>/dev/null || ls /usr/local/zig-*/zig 2>/dev/null | head -1 || true)"
resolve_cc() {
    if command -v "${triple}-gcc" >/dev/null 2>&1; then printf '%s' "${triple}-gcc"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then printf '%s' "/opt/${triple}-cross/bin/${triple}-gcc"
    elif [[ -x "/usr/local/${triple}-cross/bin/${triple}-gcc" ]]; then printf '%s' "/usr/local/${triple}-cross/bin/${triple}-gcc"
    elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then printf '%s' "musl-gcc"
    else return 1; fi
}
resolve_cxx() {
    if command -v "${triple}-g++" >/dev/null 2>&1; then printf '%s' "${triple}-g++"
    elif [[ -x "/opt/${triple}-cross/bin/${triple}-g++" ]]; then printf '%s' "/opt/${triple}-cross/bin/${triple}-g++"
    elif [[ -x "/usr/local/${triple}-cross/bin/${triple}-g++" ]]; then printf '%s' "/usr/local/${triple}-cross/bin/${triple}-g++"
    else return 1; fi
}
if cc_bin="$(resolve_cc)"; then musl_cc=("$cc_bin"); elif [[ -n "$ZIG_BIN" ]]; then musl_cc=("$ZIG_BIN" cc -target "$triple"); else musl_cc=(); fi
if cxx_bin="$(resolve_cxx)"; then musl_cxx=("$cxx_bin"); elif [[ -n "$ZIG_BIN" ]]; then musl_cxx=("$ZIG_BIN" c++ -target "$triple"); else musl_cxx=(); fi
# cargo cross-links the dynamic-musl Rust cells through a single-word gcc linker; a resolved bare
# ${triple}-gcc is required for that (zig-as-cargo-linker is out of scope here).
rust_cc="$(resolve_cc || true)"

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

ROOTFS_SIZE=4G
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    command -v resize2fs >/dev/null 2>&1 || { echo "prebuild: resize2fs required (e2fsprogs)" >&2; exit 1; }
    local before after; before=$(stat -c %s "$base_rootfs")
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown $((before/1024/1024)) -> $((after/1024/1024)) MiB for mesa/lavapipe+llvmpipe closure"
}

normalize_symlinks() {
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

# mesa software Vulkan (lavapipe) + Vulkan loader for the wgpu Vulkan backend, plus mesa-gl/egl/gallium
# (llvmpipe) for the wgpu GL backend, all musl for the target arch.
GPU_PKGS=(musl mesa-vulkan-swrast vulkan-loader vulkan-headers mesa-gl mesa-egl mesa-dri-gallium zlib
          python3 py3-numpy py3-cffi py3-sniffio)

apk_provision() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    local apk_common=(--root "$staging_root" --repositories-file "$staging_root/etc/apk/repositories"
                      --keys-dir "$staging_root/etc/apk/keys" --no-progress --no-scripts)
    echo "prebuild: apk add WebGPU render stack (${GPU_PKGS[*]}) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "${apk_common[@]}" --update-cache add "${GPU_PKGS[@]}"
    [[ -f "$staging_root/usr/lib/libvulkan_lvp.so" ]] || { echo "prebuild: mesa-vulkan-swrast (lavapipe) not provisioned" >&2; exit 3; }
    [[ -n "$(ls "$staging_root/usr/lib/libEGL.so"* 2>/dev/null)" ]] || echo "prebuild: WARN libEGL absent - wgpu GL backend may be unavailable this arch"
}

RUST_CHANNEL="${GPU_WGPU_RUST_CHANNEL:-nightly-2026-05-28-x86_64-unknown-linux-gnu}"
build_rust_carpet() {
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required to build the wgpu Rust render carpet" >&2; exit 5; }
    [[ -n "$rust_cc" ]] || { echo "prebuild: ${triple}-gcc required on PATH or /opt/${triple}-cross to cross-link the musl carpet for $arch" >&2; exit 5; }
    local bin="$staging_root/opt/cpu-wgpu-render"; mkdir -p "$bin"
    local rsbuild rsout rshome; rsbuild="$(mktemp -d)"; rsout="$(mktemp -d)"; rshome="$(mktemp -d)"
    cp -a "$RSDIR/." "$rsbuild/"
    local link_var="CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER"
    local cc_var="CC_$(echo "$rust_target" | tr '-' '_')"
    echo "prebuild: cross-build wgpu Rust render carpet -> $rust_target (dynamic musl, lavapipe/llvmpipe at runtime)"
    if ( cd "$rsbuild" && env \
            CARGO_HOME="$rshome" CARGO_TARGET_DIR="$rsout" \
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
            "$cc_var=$rust_cc" "$link_var=$rust_cc" \
            RUSTFLAGS="-C target-feature=-crt-static" \
            cargo "+$RUST_CHANNEL" build --release --locked --target "$rust_target" ) \
       && [[ -f "$rsout/$rust_target/release/wgpu_render_rust_full_api" ]]; then
        install -Dm0755 "$rsout/$rust_target/release/wgpu_render_rust_full_api" "$bin/wgpu_render_rust"
        echo "prebuild: staged wgpu_render_rust for $rust_target (dynamic musl PIE)"
    else
        echo "prebuild: wgpu Rust render carpet failed to build for $rust_target" >&2
        rm -rf "$rsbuild" "$rsout" "$rshome"; exit 5
    fi
    rm -rf "$rsbuild" "$rsout" "$rshome"
    cp "$app_dir/programs/run_all.sh" "$bin/run_all.sh"; chmod +x "$bin/run_all.sh"
}

# The four render-scenario scene cells (scene_2dui / scene_3dmodel / scene_anim / scene_codec), each its
# own wgpu-crate cargo crate mirroring wgpu_render_rust: cross-compiled to <arch>-unknown-linux-musl
# (dynamic musl, -C target-feature=-crt-static, --release --locked), staged as its own binary. Each cell
# renders offscreen and asserts every pixel against an independent closed-form reference ported from the
# GLES/Vulkan scene sources; run under both software backends by run_all.sh.
SCENE_CELLS=(scene_2dui scene_3dmodel scene_anim scene_codec)
build_scene_carpets() {
    command -v cargo >/dev/null 2>&1 || { echo "prebuild: cargo required to build the wgpu scene carpets" >&2; exit 5; }
    [[ -n "$rust_cc" ]] || { echo "prebuild: ${triple}-gcc required on PATH or /opt/${triple}-cross to cross-link the scene carpets for $arch" >&2; exit 5; }
    local bin="$staging_root/opt/cpu-wgpu-render"; mkdir -p "$bin"
    local link_var="CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER"
    local cc_var="CC_$(echo "$rust_target" | tr '-' '_')"
    local cell
    for cell in "${SCENE_CELLS[@]}"; do
        local srcdir="$CAR/$cell"
        [[ -f "$srcdir/Cargo.toml" ]] || { echo "prebuild: scene cell $cell source missing at $srcdir" >&2; exit 5; }
        local sbuild sout shome; sbuild="$(mktemp -d)"; sout="$(mktemp -d)"; shome="$(mktemp -d)"
        cp -a "$srcdir/." "$sbuild/"
        echo "prebuild: cross-build wgpu scene carpet $cell -> $rust_target (dynamic musl, lavapipe/llvmpipe at runtime)"
        if ( cd "$sbuild" && env \
                CARGO_HOME="$shome" CARGO_TARGET_DIR="$sout" \
                CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
                "$cc_var=$rust_cc" "$link_var=$rust_cc" \
                RUSTFLAGS="-C target-feature=-crt-static" \
                cargo "+$RUST_CHANNEL" build --release --locked --target "$rust_target" ) \
           && [[ -f "$sout/$rust_target/release/$cell" ]]; then
            install -Dm0755 "$sout/$rust_target/release/$cell" "$bin/$cell"
            echo "prebuild: staged $cell for $rust_target (dynamic musl PIE)"
        else
            echo "prebuild: wgpu scene carpet $cell failed to build for $rust_target" >&2
            rm -rf "$sbuild" "$sout" "$shome"; exit 5
        fi
        rm -rf "$sbuild" "$sout" "$shome"
    done
}

# libwgpu_native.so (gfx-rs wgpu-native v22, matching the Rust cell's wgpu="22") built from source for
# musl; the wgpu_render_c / wgpu_render_cpp cells #include the fetched ffi headers and link it. v22.1.0.5
# is the exact wgpu-native that the pinned wgpu-py (0.20.0) binds, so the pure-python cell's cffi cdef
# matches this .so's ABI (wgpuAdapterReference naming); the commit SHA is verified after clone.
WGPU_NATIVE_TAG="${GPU_WGPU_NATIVE_TAG:-v22.1.0.5}"
WGPU_NATIVE_SHA="${GPU_WGPU_NATIVE_SHA:-fad19f5990d8eb9a6e942eb957344957193fe66d}"
build_native_cells() {
    local bin="$staging_root/opt/cpu-wgpu-render"; mkdir -p "$bin"
    command -v git >/dev/null 2>&1 || { echo "prebuild: git required to fetch wgpu-native" >&2; exit 5; }
    [[ ${#musl_cc[@]} -gt 0 ]]  || { echo "prebuild: no host musl-cross C compiler for $triple (tried ${triple}-gcc, /opt/${triple}-cross, zig cc)" >&2; exit 5; }
    [[ ${#musl_cxx[@]} -gt 0 ]] || { echo "prebuild: no host musl-cross C++ compiler for $triple (tried ${triple}-g++, /opt/${triple}-cross, zig c++)" >&2; exit 5; }
    [[ -n "$rust_cc" ]]         || { echo "prebuild: ${triple}-gcc required to cross-link libwgpu_native.so for $arch" >&2; exit 5; }
    local wn wnout wnhome; wn="$(mktemp -d)"; wnout="$(mktemp -d)"; wnhome="$(mktemp -d)"
    echo "prebuild: fetch wgpu-native $WGPU_NATIVE_TAG (official gfx-rs) + submodules for $arch"
    git clone --depth 1 --branch "$WGPU_NATIVE_TAG" https://github.com/gfx-rs/wgpu-native "$wn" >/dev/null 2>&1 \
        || { echo "prebuild: wgpu-native clone failed ($WGPU_NATIVE_TAG)" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
    local got_sha; got_sha="$(git -C "$wn" rev-parse HEAD 2>/dev/null)"
    [[ "$got_sha" == "$WGPU_NATIVE_SHA" ]] \
        || { echo "prebuild: wgpu-native $WGPU_NATIVE_TAG SHA mismatch (got $got_sha want $WGPU_NATIVE_SHA)" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 6; }
    ( cd "$wn" && git submodule update --init --recursive >/dev/null 2>&1 ) \
        || { echo "prebuild: wgpu-native submodule init failed (ffi/webgpu-headers)" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
    local link_var="CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER"
    local cc_var="CC_$(echo "$rust_target" | tr '-' '_')"
    echo "prebuild: cross-build libwgpu_native.so (cdylib) -> $rust_target (dynamic musl)"
    if ! ( cd "$wn" && env CARGO_HOME="$wnhome" CARGO_TARGET_DIR="$wnout" \
            CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
            "$cc_var=$rust_cc" "$link_var=$rust_cc" RUSTFLAGS="-C target-feature=-crt-static" \
            cargo "+$RUST_CHANNEL" build --release --locked --target "$rust_target" ) \
       || [[ ! -f "$wnout/$rust_target/release/libwgpu_native.so" ]]; then
        echo "prebuild: libwgpu_native.so failed to build for $rust_target" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5
    fi
    install -Dm0755 "$wnout/$rust_target/release/libwgpu_native.so" "$staging_root/usr/lib/libwgpu_native.so"
    echo "prebuild: staged libwgpu_native.so ($(stat -c %s "$staging_root/usr/lib/libwgpu_native.so") bytes) for $arch"
    local inc=(-I"$wn/ffi" -I"$wn/ffi/webgpu-headers")
    # Link the staged Rust cdylib; -rpath-link lets the host linker resolve its transitive musl deps
    # that live under the staging root (not the host's own /usr/lib).
    local link_flags=(-L"$staging_root/usr/lib" -Wl,-rpath-link,"$staging_root/usr/lib:$staging_root/lib" -lwgpu_native -lm)
    "${musl_cc[@]}" -O2 -std=c11 "${inc[@]}" "$CAR/wgpu_render_c/wgpu_render_c_full_api.c" -o "$bin/wgpu_render_c" \
        "${link_flags[@]}" \
        || { echo "prebuild: wgpu_render_c failed to compile/link for $arch" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
    "${musl_cxx[@]}" -O2 -std=c++17 "${inc[@]}" "$CAR/wgpu_render_cpp/wgpu_render_cpp_full_api.cpp" -o "$bin/wgpu_render_cpp" \
        "${link_flags[@]}" \
        || { echo "prebuild: wgpu_render_cpp failed to compile/link for $arch" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
    echo "prebuild: wgpu_render_c + wgpu_render_cpp linked against libwgpu_native.so for $arch"
    # The four render-scenario scene cells in C and C++ (scene_<NAME>_c / scene_<NAME>_cpp), linking the
    # same libwgpu_native.so and #including the same fetched ffi headers as the wgpu_render_c/cpp cells.
    # Each is a binding-only port of the matching Rust scene cell (same WGSL shaders, same closed-form
    # references) and prints its own "SCENE_<NAME>_C / _CPP OK <n>" marker; run under both software
    # backends by run_all.sh.
    local scene
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        "${musl_cc[@]}" -O2 -std=c11 "${inc[@]}" "$CAR/${scene}_c/${scene}_c.c" -o "$bin/${scene}_c" \
            "${link_flags[@]}" \
            || { echo "prebuild: ${scene}_c failed to compile/link for $arch" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
        "${musl_cxx[@]}" -O2 -std=c++17 "${inc[@]}" "$CAR/${scene}_cpp/${scene}_cpp.cpp" -o "$bin/${scene}_cpp" \
            "${link_flags[@]}" \
            || { echo "prebuild: ${scene}_cpp failed to compile/link for $arch" >&2; rm -rf "$wn" "$wnout" "$wnhome"; exit 5; }
    done
    echo "prebuild: scene_{2dui,3dmodel,anim,codec}_{c,cpp} linked against libwgpu_native.so for $arch"
    rm -rf "$wn" "$wnout" "$wnhome"
}

# wgpu_render_py: pure-python wgpu-py sdist + WGPU_LIB_PATH -> the built musl libwgpu_native. wgpu-py 0.20.0
# is the release whose bundled wgpu-native pin is exactly v22.1.0.5 (wgpu/backends/wgpu_native/__init__.py
# __version__), so its cffi cdef matches this .so's ABI (wgpuAdapterReference naming). wgpu-py 0.21+ jumped
# to wgpu-native v24 (wgpuAdapterAddRef) which this v22 .so does not export. python3 + numpy + cffi from apk.
WGPU_PY_VER="${GPU_WGPU_PY_VER:-0.20.0}"
WGPU_PY_SHA256="${GPU_WGPU_PY_SHA256:-7e9eb5f3b3f6bdb88d5f9d4f254c84883a69cf1d1eddc56cabd7c21978c15373}"
provision_wgpu_py() {
    local bin="$staging_root/opt/cpu-wgpu-render"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 not provisioned - wgpu_render_py cell not wired" >&2; return 0; }
    [[ -f "$staging_root/usr/lib/libwgpu_native.so" ]] || { echo "prebuild: libwgpu_native.so absent - wgpu_render_py cell not wired"; return 0; }
    local sp; sp="$(ls -d "$staging_root"/usr/lib/python3.*/site-packages 2>/dev/null | head -1)"
    [[ -n "$sp" ]] || { echo "prebuild: site-packages not found - wgpu_render_py cell not wired"; return 0; }
    local wt; wt="$(mktemp -d)"
    echo "prebuild: fetch + verify wgpu-py sdist $WGPU_PY_VER (pure-python, official PyPI)"
    if ! curl -fsSL -o "$wt/wgpu.tar.gz" "https://files.pythonhosted.org/packages/source/w/wgpu/wgpu-${WGPU_PY_VER}.tar.gz"; then
        echo "prebuild: wgpu-py sdist download failed - required wgpu_render_py cell cannot be provisioned" >&2; rm -rf "$wt"; exit 5
    fi
    echo "$WGPU_PY_SHA256  $wt/wgpu.tar.gz" | sha256sum -c - >/dev/null 2>&1 \
        || { echo "prebuild: wgpu-py sdist SHA-256 mismatch" >&2; rm -rf "$wt"; exit 6; }
    ( cd "$wt" && tar xzf wgpu.tar.gz )
    [[ -d "$wt/wgpu-$WGPU_PY_VER/wgpu" ]] || { echo "prebuild: wgpu-py sdist layout unexpected" >&2; rm -rf "$wt"; exit 6; }
    cp -a "$wt/wgpu-$WGPU_PY_VER/wgpu" "$sp/"
    rm -rf "$wt"
    cp "$CAR/wgpu_render_py/wgpu_render_py_full_api.py" "$bin/wgpu_render_py.py"
    cat > "$bin/wgpu_render_py" <<'PYW'
#!/bin/sh
export WGPU_LIB_PATH=/usr/lib/libwgpu_native.so
mkdir -p /tmp/vkrt; export XDG_RUNTIME_DIR=/tmp/vkrt
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-wgpu-render/wgpu_render_py.py "$@"
PYW
    chmod +x "$bin/wgpu_render_py"
    echo "prebuild: wgpu_render_py -> /opt/cpu-wgpu-render/wgpu_render_py (pure-python wgpu $WGPU_PY_VER + WGPU_LIB_PATH -> musl libwgpu_native)"
}

# The four render-scenario scene cells in Python (scene_<NAME>_py), each a binding-only port of the
# matching Rust scene cell (same WGSL shaders, same closed-form references) driven by the same wgpu-py
# sdist + WGPU_LIB_PATH as wgpu_render_py. Each is staged with its own /bin/sh launcher and prints its
# own "SCENE_<NAME>_PY OK <n>" marker; run under both software backends by run_all.sh. Wired only where
# wgpu_render_py itself was wired (python3 + wgpu-py present).
provision_scene_py() {
    local bin="$staging_root/opt/cpu-wgpu-render"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: python3 absent - scene_*_py cells not wired"; return 0; }
    local sp; sp="$(ls -d "$staging_root"/usr/lib/python3.*/site-packages 2>/dev/null | head -1)"
    [[ -n "$sp" && -d "$sp/wgpu" ]] || { echo "prebuild: wgpu-py not provisioned - scene_*_py cells not wired"; return 0; }
    local scene
    for scene in scene_2dui scene_3dmodel scene_anim scene_codec; do
        [[ -f "$CAR/${scene}_py/${scene}_py.py" ]] || { echo "prebuild: ${scene}_py source missing" >&2; exit 6; }
        cp "$CAR/${scene}_py/${scene}_py.py" "$bin/${scene}_py.py"
        cat > "$bin/${scene}_py" <<PYW
#!/bin/sh
export WGPU_LIB_PATH=/usr/lib/libwgpu_native.so
mkdir -p /tmp/vkrt; export XDG_RUNTIME_DIR=/tmp/vkrt
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
exec python3 -X faulthandler /opt/cpu-wgpu-render/${scene}_py.py "\$@"
PYW
        chmod +x "$bin/${scene}_py"
    done
    echo "prebuild: scene_{2dui,3dmodel,anim,codec}_py -> /opt/cpu-wgpu-render (wgpu-py + WGPU_LIB_PATH)"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/lib" "$overlay_dir/usr/share" "$overlay_dir/opt" "$overlay_dir/usr/bin"
    cp -a "$staging_root"/usr/bin/python3* "$overlay_dir/usr/bin/" 2>/dev/null || true
    local mbin="$staging_root/opt/cpu-wgpu-render"
    : > "$mbin/expected_cells"
    # All 20 cells (4 render x {c,cpp,rust,py} minus 0 + 4 scenes x {native,c,cpp,rust,py}) are required on
    # every arch; a missing binary here means an upstream build/provision step failed silently, so hard-fail
    # instead of shrinking the manifest (which would let the gate pass on a partial run).
    for c in wgpu_render_rust wgpu_render_c wgpu_render_cpp wgpu_render_py \
             scene_2dui scene_3dmodel scene_anim scene_codec \
             scene_2dui_c scene_3dmodel_c scene_anim_c scene_codec_c \
             scene_2dui_cpp scene_3dmodel_cpp scene_anim_cpp scene_codec_cpp \
             scene_2dui_py scene_3dmodel_py scene_anim_py scene_codec_py; do
        [[ -x "$mbin/$c" ]] || { echo "prebuild: required cell $c absent at overlay time for $arch (upstream build/provision failed)" >&2; exit 5; }
        echo "$c" >> "$mbin/expected_cells"
    done
    echo "prebuild: expected_cells for $arch = $(tr '\n' ' ' < "$mbin/expected_cells")"
    cp -a "$staging_root/usr/lib/." "$overlay_dir/usr/lib/"
    cp -a "$staging_root/usr/share/vulkan" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/usr/share/glvnd" "$overlay_dir/usr/share/" 2>/dev/null || true
    cp -a "$staging_root/opt/cpu-wgpu-render" "$overlay_dir/opt/"
    ln -sf /opt/cpu-wgpu-render/run_all.sh "$overlay_dir/usr/bin/run_all.sh"
    echo "prebuild: overlay populated for $arch ($(du -sh "$overlay_dir/usr/lib" | cut -f1) libs)"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
apk_provision
build_rust_carpet
build_scene_carpets
build_native_cells
provision_wgpu_py
provision_scene_py
populate_overlay
echo "prebuild: cpu-wgpu-render overlay ready for $arch"
