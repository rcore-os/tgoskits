#!/usr/bin/env bash
# doomgeneric prebuild — 模仿 ffplay 构建流程
set -euo pipefail

# Overridable defaults for Docker container environment
: "${STARRY_ROOTFS:=/tmp/.tgos-images/rootfs-x86_64-alpine.img/rootfs-x86_64-alpine.img}"
: "${STARRY_STAGING_ROOT:=/workspace/tmp/doom-staging}"
: "${STARRY_OVERLAY_DIR:=/workspace/tmp/doom-overlay}"
: "${STARRY_ARCH:=x86_64}"

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="$STARRY_ARCH"
base_rootfs="$STARRY_ROOTFS"
staging_root="$STARRY_STAGING_ROOT"
overlay_dir="$STARRY_OVERLAY_DIR"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
apk_cache="$workspace/target/doomgeneric-apk-cache"

ensure_host_packages() {
    local missing=()

    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)

    case "$arch" in
        x86_64)  command -v qemu-x86_64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        aarch64) command -v qemu-aarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
    esac

    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required host packages: ${missing[*]}" >&2
        exit 1
    fi
}

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

install_packages() {
    local qemu_runner
    case "$arch" in
        x86_64)  qemu_runner="qemu-x86_64-static" ;;
        aarch64) qemu_runner="qemu-aarch64-static" ;;
        *) echo "unsupported arch: $arch" >&2; exit 1 ;;
    esac

    [[ -f /etc/resolv.conf ]] && cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    mkdir -p "$apk_cache"

    cat > "$staging_root/etc/apk/repositories" <<'REPO'
https://mirrors.aliyun.com/alpine/edge/main
https://mirrors.aliyun.com/alpine/edge/community
REPO

    echo "[doom] installing packages via qemu-user apk..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --update-cache --no-progress --no-scripts \
            add weston weston-backend-drm mesa-gbm \
                mesa mesa-egl mesa-gl mesa-dri-gallium mesa-utils \
                libinput libxkbcommon pixman xkeyboard-config \
                freedoom sdl2 \
                gcc make musl-dev sdl2-dev sdl2_mixer-dev

    # debugfs rdump 可能丢失执行权限，修复关键二进制和动态链接器
    echo "[doom] fixing permissions in staging root..."
    chmod -R 755 "$staging_root/lib/" 2>/dev/null || true
    chmod -R 755 "$staging_root/usr/lib/" 2>/dev/null || true
    chmod 755 "$staging_root/usr/bin/"* 2>/dev/null || true
    chmod 755 "$staging_root/bin/"* 2>/dev/null || true
    chmod 755 "$staging_root/sbin/"* 2>/dev/null || true
}

build_doomgeneric() {
    local doom_src_url="https://github.com/ozkl/doomgeneric/archive/refs/heads/master.tar.gz"
    local doom_src_cache="$workspace/target/doomgeneric-source.tar.gz"
    mkdir -p "$(dirname "$doom_src_cache")"

    echo "[doom] downloading doomgeneric source..."
    if [[ ! -f "$doom_src_cache" ]]; then
        wget -q --timeout=30 -O "$doom_src_cache" "$doom_src_url" || \
            curl -fsSL --max-time 30 -o "$doom_src_cache" "$doom_src_url"
    fi
    [[ -f "$doom_src_cache" ]] || { echo "error: failed to download doomgeneric from $doom_src_url" >&2; exit 1; }

    echo "[doom] extracting doomgeneric source..."
    local src_dir="$staging_root/tmp/doomgeneric-src"
    local host_src="/tmp/doomgeneric-src"
    local doom_bin_out="$workspace/target/.doomgeneric"
    # 清旧产物，防止静默沿用失败构建
    rm -rf "$src_dir" "$host_src" "$doom_bin_out"
    mkdir -p "$src_dir" "$host_src" "$(dirname "$doom_bin_out")" /tmp/doom-extract
    tar xzf "$doom_src_cache" -C /tmp/doom-extract
    cp -r /tmp/doom-extract/doomgeneric-master/doomgeneric/* "$src_dir/"
    # 也要复制到 host /tmp，因为 qemu-user 走 host 文件系统
    cp -r /tmp/doom-extract/doomgeneric-master/doomgeneric/* "$host_src/"
    rm -rf /tmp/doom-extract

    # 替换 CC=clang → CC=gcc
    sed -i 's/CC=clang/CC=gcc/' "$src_dir/Makefile.sdl"
    sed -i 's/CC=clang/CC=gcc/' "$host_src/Makefile.sdl"
    # 给 DG_Init 添加 renderer NULL 检查 + SDL_GetError 打印
    for s in "$src_dir/doomgeneric_sdl.c" "$host_src/doomgeneric_sdl.c"; do
        sed -i '/^  renderer =  SDL_CreateRenderer/a\  if (!renderer) {\n    fprintf(stderr, "DG_Init FATAL: SDL_CreateRenderer failed: %s\\n", SDL_GetError());\n    exit(1);\n  }' "$s"
    done

    local qemu_runner
    case "$arch" in
        x86_64)  qemu_runner="qemu-x86_64-static" ;;
        aarch64) qemu_runner="qemu-aarch64-static" ;;
        *) echo "unsupported arch: $arch" >&2; exit 1 ;;
    esac

    echo "[doom] compiling doomgeneric via qemu-user..."
    # Makefile 用反引号执行 sdl2-config，但 qemu-user 下 shebang 脚本的 exec()
    # 行为不稳定，且 sdl2-config 返回的 -I/usr/include/SDL2 是客户机路径，
    # 在宿主机上不存在。因此显式覆盖 SDL_CFLAGS/SDL_LIBS，不走反引号。
    # --sysroot: 让 GCC/ld 把系统路径前缀拼上 staging root（qemu-user -L
    #   只翻译 ELF 加载路径，不翻译 GCC/ld 的 open() 系统调用）
    # -I/-L $staging_root: 显式传宿主机可见的绝对路径给 SDL2
    # --allow-multiple-definition: dummy.o 和 i_sdlmusic.o 都定义了函数
    "$qemu_runner" -L "$staging_root" \
        /bin/sh -c "cd /tmp/doomgeneric-src && make -f Makefile.sdl clean && make -f Makefile.sdl -j4 \
        CC=gcc \
        SDL_CFLAGS=\"\" \
        SDL_LIBS=\"-lSDL2_mixer -lSDL2\" \
        CFLAGS=\"--sysroot=$staging_root -I$staging_root/usr/include/SDL2 -D_REENTRANT\" \
        LDFLAGS=\"--sysroot=$staging_root -no-pie -Wl,--allow-multiple-definition \
                  -Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1 \
                  -L$staging_root/usr/lib -L$staging_root/lib\"" 2>&1 || true

    # 检查编译产物（明确失败）
    if [[ ! -f "$src_dir/doomgeneric" && ! -f "$host_src/doomgeneric" ]]; then
        echo "[doom] ERROR: build failed!" >&2
        exit 1
    fi
    local doom_bin=""
    [[ -f "$src_dir/doomgeneric" ]] && doom_bin="$src_dir/doomgeneric"
    [[ -z "$doom_bin" && -f "$host_src/doomgeneric" ]] && doom_bin="$host_src/doomgeneric"

    cp "$doom_bin" "$doom_bin_out"
    chmod +x "$doom_bin_out"
    # 也复制到 staging root，便于 populate_overlay 解析 DT_NEEDED
    mkdir -p "$staging_root/usr/bin" && cp "$doom_bin" "$staging_root/usr/bin/doomgeneric" && chmod +x "$staging_root/usr/bin/doomgeneric"
    echo "[doom] doomgeneric built successfully (musl)"
}

# ================================================================
# Helper: copy a single file from staging root to overlay,
# resolving symlinks and handling missing files gracefully.
# ================================================================
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

# ================================================================
# Helper: find a shared library in standard library directories.
# ================================================================
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

# ================================================================
# Helper: recursively copy runtime DT_NEEDED dependencies for a
# binary or .so, transitively.
# ================================================================
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
    echo "[doom] populating overlay..."

    # === Mesa shader cache 目录（预创建避免 "No space left" 警告） ===
    mkdir -p "$overlay_dir/root/.cache/mesa_shader_cache"

    # === Weston 二进制 + 运行时 .so 依赖（自动解析 DT_NEEDED 递归复制） ===
    copy_file_to_overlay /usr/bin/weston 0755
    copy_runtime_dependencies /usr/bin/weston

    # Weston backend/plugin 模块 (libweston-14/*.so, usr/lib/weston/*)
    if [[ -d "$staging_root/usr/lib/libweston-14" ]]; then
        mkdir -p "$overlay_dir/usr/lib/libweston-14"
        find "$staging_root/usr/lib/libweston-14" -maxdepth 1 -type f | while read -r src; do
            install -Dm0644 "$src" "$overlay_dir/usr/lib/libweston-14/$(basename "$src")"
        done
    fi
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
    # 解析 libweston-14 插件的 DT_NEEDED
    if [[ -d "$overlay_dir/usr/lib/libweston-14" ]]; then
        for so in "$overlay_dir/usr/lib/libweston-14"/*.so; do
            [[ -f "$so" ]] || continue
            local rel="${so#$overlay_dir}"
            copy_runtime_dependencies "$rel" 2>/dev/null || true
        done
    fi

    # Weston example clients (weston-simple-shm, 等)
    if [[ -d "$staging_root/usr/libexec/weston" ]]; then
        mkdir -p "$overlay_dir/usr/bin"
        find "$staging_root/usr/libexec/weston" -maxdepth 1 -type f | while read -r src; do
            local name
            name="$(basename "$src")"
            install -Dm0755 "$src" "$overlay_dir/usr/bin/$name"
            echo "[doom] weston example: $name"
        done
    fi

    # === Mesa DRI / GL 库 ===
    for d in usr/lib/dri usr/lib/gallium usr/lib/gbm; do
        [[ -d "$staging_root/$d" ]] || continue
        mkdir -p "$overlay_dir/$d"
        cp -L "$staging_root/$d"/*.so "$overlay_dir/$d/" 2>/dev/null || true
    done
    # 解析 DRI 驱动的 DT_NEEDED（libgallium, libLLVM 等）
    for dri_so in "$overlay_dir/usr/lib/dri/"*.so \
                  "$overlay_dir/usr/lib/gbm/"*.so \
                  "$overlay_dir/usr/lib/gallium/"*.so; do
        [[ -f "$dri_so" ]] || continue
        local rel="${dri_so#$overlay_dir}"
        copy_runtime_dependencies "$rel" 2>/dev/null || true
    done

    # === SDL2 dlopen'd 库全面扫描 ===
    # SDL2 --enable-wayland-shared: dlopen 加载 wayland/xkbcommon
    # SDL2_mixer: dlopen 加载音频编解码库 (vorbis, ogg, mpg123, opus, flac 等)
    # Mesa: dlopen 加载 DRI/EGL/GL 驱动
    for lib in libwayland-client.so.0 libwayland-cursor.so.0 \
               libwayland-egl.so.1 libxkbcommon.so.0 \
               libxkbcommon-x11.so.0; do
        if lib_path=$(find_library_path "$lib"); then
            copy_file_to_overlay "$lib_path" 0644
            copy_runtime_dependencies "$lib_path" 2>/dev/null || true
        fi
    done
    # 全面扫描 usr/lib/ 和 lib/ 下所有与 SDL/Wayland/Mesa/音频相关的 .so
    for pattern in libSDL2* libwayland-* libxkbcommon* \
                   libEGL* libGL* libGLES* libgbm* libgallium* libglapi* \
                   libdrm* libffi* libexpat* libz* libstdc++* \
                   libvorbis* libogg* libmpg123* libopus* libFLAC* \
                   libsndfile* libpulse* libasyncns* liblzma* \
                   libpng* libjpeg* libtiff* libwebp* \
                   libpthread* librt* libdl* libm* libxcrypt*; do
        for dir in "$staging_root/usr/lib" "$staging_root/lib"; do
            for f in "$dir"/$pattern; do
                [[ -f "$f" ]] || continue
                local base; base="$(basename "$f")"
                local target_dir="$overlay_dir/usr/lib"
                if [[ ! -f "$target_dir/$base" ]]; then
                    cp -L "$f" "$target_dir/$base" && chmod 0644 "$target_dir/$base"
                fi
            done
        done
    done
    # 确保 SDL2_mixer 的 dlopen 加载器能找到音频插件
    for subdir in usr/lib/sdl2 usr/lib/alsa-lib usr/lib/pulseaudio; do
        if [[ -d "$staging_root/$subdir" ]]; then
            mkdir -p "$overlay_dir/$subdir"
            cp -L "$staging_root/$subdir"/*.so "$overlay_dir/$subdir/" 2>/dev/null || true
        fi
    done

    # === 动态链接器 (musl) ===
    [[ -f "$staging_root/lib/ld-musl-x86_64.so.1" ]] && \
        install -Dm0644 "$staging_root/lib/ld-musl-x86_64.so.1" "$overlay_dir/lib/ld-musl-x86_64.so.1"

    # === doomgeneric ===
    local doom_bin_out="$workspace/target/.doomgeneric"
    [[ -f "$doom_bin_out" ]] || { echo "error: doomgeneric binary missing" >&2; exit 1; }
    install -Dm0755 "$doom_bin_out" "$overlay_dir/usr/bin/doomgeneric"
    # 确保 staging root 里有 doomgeneric，以便解析 DT_NEEDED 复制 SDL2 等库
    if [[ ! -f "$staging_root/usr/bin/doomgeneric" ]]; then
        mkdir -p "$staging_root/usr/bin"
        cp "$doom_bin_out" "$staging_root/usr/bin/doomgeneric"
    fi
    copy_runtime_dependencies /usr/bin/doomgeneric
    install -Dm0755 "$app_dir/test_doom.sh" "$overlay_dir/usr/bin/test_doom.sh"

    # === mesa-utils / weston-info ===
    for bin in eglinfo weston-info; do
        [[ -f "$staging_root/usr/bin/$bin" ]] && install -Dm0755 "$staging_root/usr/bin/$bin" "$overlay_dir/usr/bin/$bin"
    done

    # === XKB 数据 ===
    for share in usr/share/X11/xkb usr/share/X11/locale; do
        [[ -d "$staging_root/$share" ]] || continue
        (cd "$staging_root/$share" && find . -type f 2>/dev/null | while read -r f; do
            install -Dm0644 "$staging_root/$share/$f" "$overlay_dir/$share/$f"
        done)
    done

    # === libinput 数据 ===
    if [[ -d "$staging_root/usr/share/libinput" ]]; then
        mkdir -p "$overlay_dir/usr/share/libinput"
        find "$staging_root/usr/share/libinput" -type f | while read -r f; do
            install -Dm0644 "$f" "$overlay_dir/usr/share/libinput/$(basename "$f")"
        done
    fi

    # === Freedoom WAD ===
    if [[ -d "$staging_root/usr/share/games/doom" ]]; then
        mkdir -p "$overlay_dir/usr/share/games/doom"
        find "$staging_root/usr/share/games/doom" -name '*.wad' -type f | while read -r f; do
            install -Dm0644 "$f" "$overlay_dir/usr/share/games/doom/$(basename "$f")"
        done
    fi

    echo "[doom] overlay ready"
}

ensure_host_packages
extract_base_rootfs
install_packages
build_doomgeneric
populate_overlay
rm -f "$workspace/target/.doomgeneric"
