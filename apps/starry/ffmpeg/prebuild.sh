#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/ffmpeg-apk-cache"

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

    command -v apk >/dev/null 2>&1 || missing+=(apk-tools)
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)

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

install_ffmpeg_package() {
    mkdir -p "$apk_cache"

    # Override repositories if the ones baked into the rootfs are unreachable
    local repo_file="$staging_root/etc/apk/repositories"
    if [[ -f "$repo_file" ]]; then
        local first_url
        first_url="$(head -1 "$repo_file")"
        local check_url="${first_url}/x86_64/APKINDEX.tar.gz"
        local http_code
        http_code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$check_url" 2>/dev/null || true)"
        if [[ "$http_code" != "200" ]]; then
            echo "[ffmpeg prebuild] original mirror unreachable (HTTP $http_code), switching to dl-cdn.alpinelinux.org"
            local alpine_ver
            alpine_ver="$(grep -oP 'v\d+\.\d+' "$repo_file" | head -1)"
            [[ -z "$alpine_ver" ]] && alpine_ver="v3.21"
            cat > "$repo_file" << REPOEOF
https://dl-cdn.alpinelinux.org/alpine/${alpine_ver}/main
https://dl-cdn.alpinelinux.org/alpine/${alpine_ver}/community
REPOEOF
        fi
    fi

    echo "[ffmpeg prebuild] installing ffmpeg, ffmpeg-libs and python3 via host apk..."
    apk --root "$staging_root" \
        --cache-dir "$apk_cache" \
        --update-cache \
        --no-progress \
        --no-scripts \
        add ffmpeg ffmpeg-libs python3
}

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "error: missing guest file after FFmpeg package install: $guest_path" >&2
        exit 1
    fi

    if [[ -L "$source" ]]; then
        source="$(readlink -f "$source")"
    fi

    install -Dm"$mode" "$source" "$target"
}

find_library_path() {
    local library="$1"
    local dir

    for dir in lib usr/lib usr/local/lib; do
        if [[ -e "$staging_root/$dir/$library" ]]; then
            printf '/%s/%s\n' "$dir" "$library"
            return 0
        fi
    done

    # Search non-standard library paths (e.g. /usr/lib/pulseaudio/)
    local found
    found="$(find "$staging_root/usr/lib" -name "$library" -print -quit 2>/dev/null)"
    if [[ -n "$found" ]]; then
        local rel="${found#"$staging_root"}"
        printf '%s\n' "$rel"
        return 0
    fi

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

generate_test_media() {
    # Generate small test media files using ffmpeg on the host
    # These are placed in the overlay so they're available inside QEMU
    local test_media_dir="$overlay_dir/usr/share/ffmpeg-test-media"
    mkdir -p "$test_media_dir"

    if command -v ffmpeg >/dev/null 2>&1; then
        # Generate a 2-second test video (raw video, no audio) - MP4 container
        ffmpeg -y -f lavfi -i "color=c=red:s=160x120:d=2" \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            "$test_media_dir/test_160x120.mp4" 2>/dev/null || true

        # Generate a 2-second test video with audio
        ffmpeg -y -f lavfi -i "color=c=blue:s=160x120:d=2" \
            -f lavfi -i "sine=frequency=440:duration=2" \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            -c:a aac -b:a 64k \
            "$test_media_dir/test_av.mp4" 2>/dev/null || true

        # Generate a 2-second test audio (WAV format)
        ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
            -c:a pcm_s16le \
            "$test_media_dir/test_audio.wav" 2>/dev/null || true

        # Generate a 2-second test audio (MP3 format)
        ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
            -c:a libmp3lame -b:a 128k \
            "$test_media_dir/test_audio.mp3" 2>/dev/null || true

        # Generate a 2-second test video in MKV container
        ffmpeg -y -f lavfi -i "color=c=green:s=160x120:d=2" \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            "$test_media_dir/test_160x120.mkv" 2>/dev/null || true

        # Generate a 2-second test video in AVI container
        ffmpeg -y -f lavfi -i "color=c=yellow:s=160x120:d=2" \
            -c:v mpeg4 -q:v 10 \
            "$test_media_dir/test_160x120.avi" 2>/dev/null || true

        echo "Generated test media files in $test_media_dir"
    else
        echo "WARNING: ffmpeg not found on host, test media files will not be pre-generated"
        echo "         Tests will use synthetic data generated inside QEMU"
    fi
}

populate_overlay() {
    copy_file_to_overlay /usr/bin/ffmpeg 0755
    copy_file_to_overlay /usr/bin/ffprobe 0755
    copy_runtime_dependencies /usr/bin/ffmpeg /usr/bin/ffprobe

    # Copy python3 and its standard library for HTTP server in network tests
    if [[ -e "$staging_root/usr/bin/python3" ]]; then
        copy_file_to_overlay /usr/bin/python3 0755
        copy_runtime_dependencies /usr/bin/python3
        # python3 needs its standard library for http.server module
        local py_ver
        py_ver="$(basename "$(find "$staging_root/usr/lib" -maxdepth 1 -name 'python3*' -type d | head -1)")"
        if [[ -n "$py_ver" && -d "$staging_root/usr/lib/$py_ver" ]]; then
            local src="$staging_root/usr/lib/$py_ver"
            local target="$overlay_dir/usr/lib/$py_ver"
            mkdir -p "$target"
            # Copy stdlib, skip bulky non-essential directories
            find "$src" -maxdepth 1 -mindepth 1 \
                ! -name 'test' ! -name 'tests' \
                ! -name 'ensurepip' ! -name 'idlelib' \
                ! -name 'tkinter' ! -name 'turtledemo' \
                ! -name 'distutils' ! -name 'lib2to3' \
                ! -name 'config-*' \
                -exec cp -r {} "$target/" \;
        fi
    fi

    # Copy test scripts into overlay
    install -Dm0755 "$app_dir/test_ffmpeg.sh" "$overlay_dir/usr/bin/test_ffmpeg.sh"
    install -Dm0755 "$app_dir/ffmpeg-smoke-tests.sh" "$overlay_dir/usr/bin/ffmpeg-smoke-tests.sh"
    install -Dm0755 "$app_dir/ffmpeg-basic-tests.sh" "$overlay_dir/usr/bin/ffmpeg-basic-tests.sh"
    install -Dm0755 "$app_dir/ffmpeg-thread-tests.sh" "$overlay_dir/usr/bin/ffmpeg-thread-tests.sh"
    install -Dm0755 "$app_dir/ffmpeg-codec-tests.sh" "$overlay_dir/usr/bin/ffmpeg-codec-tests.sh"
    install -Dm0755 "$app_dir/ffmpeg-network-tests.sh" "$overlay_dir/usr/bin/ffmpeg-network-tests.sh"

    # Generate and copy test media files
    generate_test_media
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
install_ffmpeg_package
populate_overlay
