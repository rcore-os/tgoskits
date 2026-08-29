#!/usr/bin/env bash
# StarryOS VirGL 测试 - 宿主机预构建脚本
# 用 qemu-user-static 在宿主机直接安装 Alpine 包到 rootfs
# 参照 qt-calc/prebuild.sh 的架构
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:-x86_64}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/virgl-apk-cache"

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

    case "$arch" in
        aarch64)     command -v qemu-aarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        riscv64)     command -v qemu-riscv64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        x86_64)      command -v qemu-x86_64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        loongarch64) command -v qemu-loongarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
    esac

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    echo "[virgl prebuild] 缺少宿主工具: ${missing[*]}"
    if command -v pacman >/dev/null 2>&1; then
        echo "[virgl prebuild] 请安装: sudo pacman -S ${missing[*]}"
    elif command -v apt-get >/dev/null 2>&1; then
        sudo apt-get update && sudo apt-get install -y "${missing[@]}"
    fi
    exit 1
}

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

resize_rootfs() {
    local img="$1"
    local target_mib="$2"

    # 先强制检查文件系统。dirty 状态下 resize2fs 拒绝扩容，而 e2fsck
    # 进入交互提问时在非交互 stdin 下会静默失败——必须 -y 自动应答。
    # e2fsck 退出码：0=干净，1/2=已修正需重跑确认，>=3=真错误。第 1 遍
    # 修复过错误的镜像必然返回 1，不能当成硬失败；必须 loop 到 0。
    echo "[virgl prebuild] 检查 rootfs 文件系统..."
    local fs_clean=0
    for attempt in 1 2 3; do
        if e2fsck -f -y "$img" 2>&1; then
            fs_clean=1
            break
        fi
        local rc=$?
        if [ "$rc" -ge 3 ]; then
            echo "error: e2fsck -f 无法修复: $img (exit=$rc)" >&2
            exit 1
        fi
        echo "[virgl prebuild] e2fsck 第 ${attempt} 遍已修正错误，重跑确认..."
    done
    if [ "$fs_clean" -ne 1 ]; then
        echo "error: e2fsck -f 反复修正仍不干净: $img" >&2
        exit 1
    fi

    # 扩容判断以"内层文件系统实际大小"为准，而不是镜像文件大小。
    # 镜像文件 5G 但内层 fs 只有 2G 时（例如某次扩容失败后一直复用
    # 旧镜像），按文件大小判断会提前 return，fs 将永远长不起来，
    # 最终 apk 灌装把 2G 写满导致 overlay 注入失败。
    local fs_mib
    fs_mib=$(dumpe2fs -h "$img" 2>/dev/null | awk -F: '/^Block count:/{gsub(/[^0-9]/,"",$2); print int($2/256)}')
    if [ "${fs_mib:-0}" -ge "$target_mib" ]; then
        return
    fi

    local file_mib extra
    file_mib=$(stat --format=%s "$img" 2>/dev/null | awk '{print int($1/1048576)}')
    extra=$((target_mib - file_mib))
    if [ "$extra" -gt 0 ]; then
        echo "[virgl prebuild] 扩容 rootfs: ${fs_mib}M → ${target_mib}M (+${extra}M)..."
        dd if=/dev/zero bs=1M count="$extra" >> "$img" 2>/dev/null
    fi
    resize2fs "$img" >/dev/null || {
        echo "error: resize2fs 失败: $img" >&2
        exit 1
    }
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

    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    mkdir -p "$apk_cache"

    # 使用华为云镜像 + edge/testing（glmark2 在 testing 仓库）
    # v3.23 的 mesa 打包包含 pipe_virgl.so（virgl 驱动），v3.22 缺失 → llvmpipe 回退
    cat > "$staging_root/etc/apk/repositories" <<'REPO'
https://mirrors.huaweicloud.com/alpine/v3.23/main
https://mirrors.huaweicloud.com/alpine/v3.23/community
https://dl-cdn.alpinelinux.org/alpine/edge/testing
REPO

    # 下载并解压 zlib（apk 运行需要）
    local zlib_url="https://mirrors.huaweicloud.com/alpine/v3.23/main/${arch}/zlib-1.3.2-r0.apk"
    local zlib_apk="$apk_cache/zlib-1.3.2-r0.apk"
    if [[ ! -f "$zlib_apk" ]]; then
        echo "[virgl prebuild] 下载 zlib..."
        wget -q --timeout=30 -O "$zlib_apk" "$zlib_url" || curl -fsSL --connect-timeout 15 --max-time 30 -o "$zlib_apk" "$zlib_url" || true
    fi
    if [[ -f "$zlib_apk" ]] && [[ -s "$zlib_apk" ]]; then
        tar xzf "$zlib_apk" -C "$staging_root" --no-same-owner 2>/dev/null || true
    fi

    echo "[virgl prebuild] 安装 Weston + Mesa + 测试工具..."
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
            add weston weston-backend-drm weston-shell-desktop weston-terminal \
                mesa-dri-gallium mesa-egl mesa-gbm mesa-gles mesa-demos mesa-dev\
                foot font-noto seatd dbus glmark2

    # 全量升级到仓库最新(v3.23)。apk add 对已安装包不会升级,而基础镜像烘焙的是
    # 旧快照:mesa 25.1.9 缺 virgl 驱动(v3.23 的 25.2.7 才编译进 libgallium 单体)、
    # wayland 1.23.1 缺 mesa 25.2.7 libEGL 需要的 wl_display_dispatch_queue_timeout
    # (wayland 1.24 新增)。不升级 → gbm 建设备失败 / EGL relocation 失败。
    echo "[virgl prebuild] 全量升级软件包(启用 virgl)..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --no-progress \
            --no-scripts \
            upgrade
}

populate_overlay() {
    echo "[virgl prebuild] 复制 usr/ 到 overlay..."
    (cd "$staging_root" && find usr \( -type f -o -type l \) | while read -r rel; do
        local src="$staging_root/$rel"
        local target="$overlay_dir/$rel"
        mkdir -p "$(dirname "$target")"
        rm -f "$target" 2>/dev/null || true
        cp -d "$src" "$target" 2>/dev/null || true
    done)

    if [[ -d "$staging_root/lib" ]]; then
        echo "[virgl prebuild] 复制 lib/ 到 overlay..."
        (cd "$staging_root" && find lib \( -type f -o -type l \) | while read -r rel; do
            local src="$staging_root/$rel"
            local target="$overlay_dir/$rel"
            mkdir -p "$(dirname "$target")"
            rm -f "$target" 2>/dev/null || true
            cp -d "$src" "$target" 2>/dev/null || true
        done)
    fi

    # 注入测试脚本
    install -Dm0755 "$app_dir/runner.sh" "$overlay_dir/usr/local/bin/virgl-runner.sh"

    # 注入 weston 配置
    mkdir -p "$overlay_dir/root/.config"
    cat > "$overlay_dir/root/.config/weston.ini" <<'WEOF'
[core]
backend=drm-backend.so
idle-time=0
xwayland=false

[output]
name=Virtual-1
mode=preferred

[shell]
locking=false

[keyboard]
keymap_layout=us
WEOF

    # 注入设备初始化脚本（开机自动启动 seatd/dbus）
    mkdir -p "$overlay_dir/etc/local.d"
    cat > "$overlay_dir/etc/local.d/virgl-start.start" <<'LDEOF'
#!/bin/sh
setup-devd udev 2>/dev/null || true
rc-service seatd start 2>/dev/null || true
rc-service dbus start 2>/dev/null || true
LDEOF
    chmod +x "$overlay_dir/etc/local.d/virgl-start.start"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
resize_rootfs "$base_rootfs" 5120
extract_base_rootfs
install_packages
populate_overlay

echo "[virgl prebuild] 完成"
