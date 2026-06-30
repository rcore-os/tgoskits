#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:-x86_64}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/ffplay-apk-cache"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

ensure_host_packages() {
    local missing=()

    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)
    command -v wget >/dev/null 2>&1 || missing+=(wget)
    command -v ffmpeg >/dev/null 2>&1 || missing+=(ffmpeg)
    command -v ffprobe >/dev/null 2>&1 || missing+=(ffmpeg)

    case "$arch" in
        aarch64)     command -v qemu-aarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        riscv64)     command -v qemu-riscv64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        x86_64)      command -v qemu-x86_64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        loongarch64) command -v qemu-loongarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
    esac

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "error: missing required host packages and apt-get is unavailable: ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

install_packages() {
    local qemu_runner
    case "$arch" in
        aarch64)     qemu_runner="qemu-aarch64-static" ;;
        riscv64)     qemu_runner="qemu-riscv64-static" ;;
        x86_64)      qemu_runner="qemu-x86_64-static" ;;
        loongarch64) qemu_runner="qemu-loongarch64-static" ;;
        *)           echo "error: unsupported arch: $arch" >&2; exit 1 ;;
    esac

    if ! command -v "$qemu_runner" >/dev/null 2>&1; then
        echo "error: $qemu_runner not found" >&2
        exit 1
    fi

    # Copy host DNS config so apk can resolve hostnames inside qemu-user.
    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    mkdir -p "$apk_cache"

    # Use Alibaba Cloud mirror for faster downloads in China.
    cat > "$staging_root/etc/apk/repositories" <<'REPO'
https://mirrors.aliyun.com/alpine/edge/main
https://mirrors.aliyun.com/alpine/edge/community
REPO

    echo "[ffplay prebuild] installing weston and ffplay via qemu-user apk..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --update-cache \
            --no-progress \
            --no-scripts \
            add weston weston-backend-drm mesa-gbm \
                mesa mesa-egl mesa-gl mesa-dri-gallium mesa-utils \
                libinput libxkbcommon pixman xkeyboard-config \
                ffplay sdl2 ffmpeg wget
}

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "warning: skipping missing file: $guest_path" >&2
        return 0
    fi

    if [[ -L "$source" ]]; then
        source="$(readlink -f "$source")"
    fi

    install -Dm"$mode" "$source" "$target"
}

find_library_path() {
    local library="$1"
    local dir

    for dir in lib usr/lib usr/local/lib usr/lib/pulseaudio usr/lib/dri; do
        if [[ -e "$staging_root/$dir/$library" ]]; then
            printf '/%s/%s\n' "$dir" "$library"
            return 0
        fi
    done

    return 1
}

copy_runtime_dependencies() {
    local pending=("$@")
    local seen=" "
    local guest_path library

    while [[ ${#pending[@]} -gt 0 ]]; do
        guest_path="${pending[0]}"
        pending=("${pending[@]:1}")

        if [[ "$seen" == *" $guest_path "* ]]; then
            continue
        fi
        seen+="$guest_path "

        while IFS= read -r library; do
            local library_path
            if ! library_path="$(find_library_path "$library")"; then
                continue
            fi
            copy_file_to_overlay "$library_path" 0644
            pending+=("$library_path")
        done < <(
            readelf -d "$staging_root$guest_path" 2>/dev/null |
                sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p'
        )
    done
}

populate_overlay() {
    # Weston compositor binary
    copy_file_to_overlay /usr/bin/weston 0755

    # Weston backend and plugin modules (resolve symlinks — inject_overlay
    # only supports regular files and directories, not symlinks).
    if [[ -d "$staging_root/usr/lib/libweston-14" ]]; then
        mkdir -p "$overlay_dir/usr/lib/libweston-14"
        find "$staging_root/usr/lib/libweston-14" -maxdepth 1 -type f | while read -r src; do
            install -Dm0644 "$src" "$overlay_dir/usr/lib/libweston-14/$(basename "$src")"
        done
    fi

    # Weston shared libraries and plugins (includes shell plugins like
    # desktop-shell.so, kiosk-shell.so, etc. that may live in subdirs).
    if [[ -d "$staging_root/usr/lib/weston" ]]; then
        (cd "$staging_root" && find usr/lib/weston \( -type f -o -type l \) | while read -r rel; do
            local src="$staging_root/$rel"
            local target="$src"
            if [[ -L "$src" ]]; then
                target="$(readlink -f "$src")"
            fi
            install -Dm0644 "$target" "$overlay_dir/$rel"
        done)
    fi

    # Resolve runtime .so dependencies for weston binary
    copy_runtime_dependencies /usr/bin/weston

    # Weston example clients (weston-simple-shm, weston-simple-egl, etc.)
    # In Alpine they live under /usr/libexec/weston/ — copy to /usr/bin/ for convenience.
    if [[ -d "$staging_root/usr/libexec/weston" ]]; then
        mkdir -p "$overlay_dir/usr/bin"
        find "$staging_root/usr/libexec/weston" -maxdepth 1 -type f | while read -r src; do
            local name
            name="$(basename "$src")"
            install -Dm0755 "$src" "$overlay_dir/usr/bin/$name"
            echo "[ffplay prebuild] weston example: $name"
        done
    fi

    # Copy libweston plugin .so deps too
    if [[ -d "$overlay_dir/usr/lib/libweston-14" ]]; then
        for so in "$overlay_dir/usr/lib/libweston-14"/*.so; do
            [[ -f "$so" ]] || continue
            local rel="${so#$overlay_dir}"
            copy_runtime_dependencies "$rel" 2>/dev/null || true
        done
    fi

    # libinput quirks data — Weston's drm-backend needs /usr/share/libinput/*.quirks
    if [[ -d "$staging_root/usr/share/libinput" ]]; then
        mkdir -p "$overlay_dir/usr/share/libinput"
        find "$staging_root/usr/share/libinput" -type f | while read -r src; do
            install -Dm0644 "$src" "$overlay_dir/usr/share/libinput/$(basename "$src")"
        done
    fi
    if [[ -d "$staging_root/etc/libinput" ]]; then
        mkdir -p "$overlay_dir/etc/libinput"
        find "$staging_root/etc/libinput" -type f | while read -r src; do
            install -Dm0644 "$src" "$overlay_dir/etc/libinput/$(basename "$src")"
        done
    fi

    # XKB keyboard data — xkbcommon needs /usr/share/X11/xkb/ to compile keymaps
    if [[ -d "$staging_root/usr/share/X11/xkb" ]]; then
        mkdir -p "$overlay_dir/usr/share/X11/xkb"
        (cd "$staging_root/usr/share/X11/xkb" && find . -type f | while read -r f; do
            install -Dm0644 "$staging_root/usr/share/X11/xkb/$f" "$overlay_dir/usr/share/X11/xkb/$f"
        done)
    fi

    # XKB compose/locale data — without this xkbcommon reports
    # "Failed to load XKB compose file" which may cause null deref in SDL2
    if [[ -d "$staging_root/usr/share/X11/locale" ]]; then
        mkdir -p "$overlay_dir/usr/share/X11/locale"
        (cd "$staging_root/usr/share/X11/locale" && find . -type f | while read -r f; do
            install -Dm0644 "$staging_root/usr/share/X11/locale/$f" "$overlay_dir/usr/share/X11/locale/$f"
        done)
        echo "[ffplay prebuild] XKB compose data: $(find "$overlay_dir/usr/share/X11/locale" -type f | wc -l) files"
    else
        echo "[ffplay prebuild] WARNING: /usr/share/X11/locale not in staging root"
    fi

    # ffplay + ffmpeg + wget for video playback test
    copy_file_to_overlay /usr/bin/ffplay 0755
    copy_file_to_overlay /usr/bin/ffmpeg 0755
    copy_file_to_overlay /usr/bin/ffprobe 0755
    copy_file_to_overlay /usr/bin/wget 0755
    copy_runtime_dependencies /usr/bin/ffplay
    copy_runtime_dependencies /usr/bin/ffmpeg
    copy_runtime_dependencies /usr/bin/wget

    # ================================================================
    # SDL2 Wayland dlopen'd shared libraries
    # Alpine SDL2 is built with --enable-wayland-shared (default).
    # Its Wayland video driver uses dlopen at runtime to load:
    #   - wayland-client/cursor/egl  (Wayland protocol)
    #   - xkbcommon                  (keyboard keymap compilation)
    #   - Mesa EGL/GL/GLES          (rendering via wl_egl_window)
    #
    # These are NOT in DT_NEEDED of ffplay or SDL2, so
    # copy_runtime_dependencies on ffplay won't catch them.
    # We copy them explicitly, then resolve their transitive
    # DT_NEEDED deps recursively.
    # ================================================================

    # 1) Wayland protocol + xkbcommon (dlopen'd by SDL2 wayland backend)
    for lib in libwayland-client.so.0 libwayland-cursor.so.0 \
               libwayland-egl.so.1 libxkbcommon.so.0 \
               libxkbcommon-x11.so.0; do
        if lib_path=$(find_library_path "$lib"); then
            copy_file_to_overlay "$lib_path" 0644
            echo "[ffplay prebuild] sdl-wayland dlopen: $lib"
            # Resolve transitive DT_NEEDED deps (e.g. libffi for wayland-client)
            copy_runtime_dependencies "$lib_path" 2>/dev/null || true
        else
            echo "[ffplay prebuild] WARNING: $lib not found in staging root"
        fi
    done

    # 2) Mesa EGL/GL/GLES (dlopen'd by SDL2 wayland backend for rendering)
    for lib in libEGL.so.1 libGL.so.1 libGLESv2.so.2 libGLESv1_CM.so.1; do
        if lib_path=$(find_library_path "$lib"); then
            copy_file_to_overlay "$lib_path" 0644
            echo "[ffplay prebuild] sdl-wayland mesa: $lib"
        else
            echo "[ffplay prebuild] WARNING: $lib not found in staging root"
        fi
    done
    # Resolve transitive deps for Mesa libs (libdrm, libglapi, etc.)
    for lib in libEGL.so.1 libGL.so.1; do
        if lib_path=$(find_library_path "$lib"); then
            copy_runtime_dependencies "$lib_path" 2>/dev/null || true
        fi
    done

    # 3) GBM buffer management
    if lib_path=$(find_library_path libgbm.so.1); then
        copy_file_to_overlay "$lib_path" 0644
        echo "[ffplay prebuild] mesa lib: libgbm.so.1"
    fi

    # 4) Mesa DRI drivers (dlopen'd by Mesa at runtime, not linked)
    # Also copy GBM DRI loader driver (dri_gbm.so) which Mesa's GBM backend needs
    for dri_dir in usr/lib/dri usr/lib/xorg/modules/dri usr/lib/gbm; do
        if [[ -d "$staging_root/$dri_dir" ]]; then
            mkdir -p "$overlay_dir/$dri_dir"
            cp -L "$staging_root/$dri_dir"/*.so "$overlay_dir/$dri_dir/" 2>/dev/null || true
            echo "[ffplay prebuild] mesa dri drivers: $(ls "$overlay_dir/$dri_dir/"*.so 2>/dev/null | xargs -n1 basename)"
        fi
    done

    # Resolve transitive DT_NEEDED for DRI drivers (libgallium, libLLVM, etc.)
    # These are dlopen'd by Mesa and their deps are not caught by libEGL's scan.
    # NOTE: we scan all three DRI directories (dri, gbm, xorg/modules/dri) because
    # Mesa's GBM loader (dri_gbm.so) and xorg module DRI drivers have DT_NEEDED
    # chains that differ from the main DRI drivers.  In practice their deps
    # (libdrm, libglapi, libgallium, etc.) are already pulled in by the libEGL
    # or dri scan, but scanning all three keeps us correct if a future Alpine
    # version adds a unique DT_NEEDED to one of these drivers.
    for dri_so in "$overlay_dir/usr/lib/dri/"*.so \
                  "$overlay_dir/usr/lib/gbm/"*.so \
                  "$overlay_dir/usr/lib/xorg/modules/dri/"*.so; do
        [[ -f "$dri_so" ]] || continue
        local rel="${dri_so#$overlay_dir}"
        copy_runtime_dependencies "$rel" 2>/dev/null || true
    done

    # 5) mesa-utils (eglinfo) — for EGL diagnostics
    copy_file_to_overlay /usr/bin/eglinfo 0755 2>/dev/null || true
    copy_runtime_dependencies /usr/bin/eglinfo 2>/dev/null || true

    # 6) weston-info — verify Wayland globals (wl_drm, linux_dmabuf, etc.)
    copy_file_to_overlay /usr/bin/weston-info 0755 2>/dev/null || true

    # 7) Comprehensive Mesa/GL/DRI sweep — copy ALL Mesa-related .so files
    # from every possible location, including ones dlopen'd dynamically.
    for subdir in dri gallium pipe libgl gbm xorg/modules/dri; do
        if [[ -d "$staging_root/usr/lib/$subdir" ]]; then
            mkdir -p "$overlay_dir/usr/lib/$subdir"
            cp -L "$staging_root/usr/lib/$subdir"/*.so "$overlay_dir/usr/lib/$subdir/" 2>/dev/null || true
        fi
    done
    # Also sweep /usr/lib/ for any Mesa/GL libraries not in subdirectories
    for pattern in libEGL* libGL* libGLES* libgbm* libgallium* libglapi* \
                   libLLVM* libdrm* libX11* libxcb* libwayland* libffi* \
                   libxkbcommon* libstdc++* libexpat* libz*; do
        for f in "$staging_root/usr/lib"/$pattern; do
            [[ -f "$f" ]] || continue
            local base
            base="$(basename "$f")"
            if [[ ! -f "$overlay_dir/usr/lib/$base" ]]; then
                cp -L "$f" "$overlay_dir/usr/lib/$base" && chmod 0644 "$overlay_dir/usr/lib/$base"
                echo "[ffplay prebuild] sweep extra: $base"
            fi
        done
    done
    # Test script
    install -Dm0755 "$app_dir/test_ffplay.sh" "$overlay_dir/usr/bin/test_ffplay.sh"

    # Sample video — downloaded on the host during build, included in the
    # overlay so the guest can play it without internet access.
    local video_url="${STARRY_VIDEO_URL:-https://media.w3.org/2010/05/sintel/trailer.mp4}"
    local video_dst="$overlay_dir/usr/share/test.mp4"
    mkdir -p "$overlay_dir/usr/share"
    if [[ ! -f "$video_dst" ]] || [[ ! -s "$video_dst" ]]; then
        echo "[ffplay prebuild] downloading sample video..."
        if command -v wget >/dev/null 2>&1; then
            # Try HTTPS first, fallback to HTTP if SSL fails
            if wget -4 -v --dns-timeout=10 --timeout=120 --no-check-certificate -O "$video_dst" "$video_url" 2>&1 && \
               [[ -s "$video_dst" ]]; then
                echo "[ffplay prebuild] downloaded video: $(wc -c < "$video_dst") bytes"
            else
                rm -f "$video_dst"
                echo "[ffplay prebuild] ERROR: video download failed" >&2
            fi
        else
            echo "[ffplay prebuild] ERROR: wget not found on host" >&2
        fi

        # Compress to 160p for faster playback (skip if already small or no ffmpeg)
        if command -v ffmpeg >/dev/null 2>&1 && [[ -s "$video_dst" ]]; then
            local dst_size
            dst_size=$(wc -c < "$video_dst")
            if [[ "$dst_size" -gt 50000 ]]; then
                local compressed="$overlay_dir/usr/share/test.mp4.tmp.mp4"
                echo "[ffplay prebuild] compressing to 160p..."
                if ffmpeg -y -i "$video_dst" \
                    -vf "scale=284:160:flags=fast_bilinear" \
                    -r 5 -c:v libx264 -preset ultrafast -b:v 100k -pix_fmt yuv420p \
                    -an "$compressed" >/dev/null 2>&1 && \
                    [[ -s "$compressed" ]]; then
                    mv "$compressed" "$video_dst"
                    echo "[ffplay prebuild] compressed: $(wc -c < "$video_dst") bytes"
                else
                    echo "[ffplay prebuild] ffmpeg compression failed, keeping original"
                    rm -f "$compressed"
                fi
            else
                echo "[ffplay prebuild] video already small ($dst_size bytes), skipping compression"
            fi
        fi
    fi

    # Final validation
    if [[ ! -s "$video_dst" ]]; then
        echo "[ffplay prebuild] FATAL: no valid video file at $video_dst" >&2
        exit 1
    fi
    echo "[ffplay prebuild] final video: $(wc -c < "$video_dst") bytes"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
install_packages
populate_overlay
