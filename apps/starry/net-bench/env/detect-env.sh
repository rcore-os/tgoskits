#!/usr/bin/env bash
# env/detect-env.sh - 自动检测运行环境并输出推荐配置
#
# 检测内容：
#   - 平台类型（WSL2 / 裸 Linux）
#   - CPU 架构（x86_64 / aarch64）
#   - KVM 可用性
#   - vhost-net 可用性
#   - 推荐的测试配置
#
# 输出：JSON 格式（便于脚本解析）或人类可读格式

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_FORMAT="${1:-json}"  # json | human

# 检测平台类型
detect_platform() {
    if grep -qi microsoft /proc/version 2>/dev/null; then
        echo "wsl2"
    else
        echo "native-linux"
    fi
}

# 检测 CPU 架构
detect_arch() {
    uname -m
}

# 检测 KVM 可用性
check_kvm() {
    if [[ ! -e /dev/kvm ]]; then
        echo "false"
        return
    fi

    if [[ -r /dev/kvm ]] && [[ -w /dev/kvm ]]; then
        echo "true"
    else
        # KVM 设备存在但无权限
        echo "no-permission"
    fi
}

# 检测 vhost-net 可用性
check_vhost() {
    if [[ -e /dev/vhost-net ]]; then
        echo "true"
    else
        # 尝试加载模块
        if modprobe vhost_net 2>/dev/null; then
            echo "true"
        else
            echo "false"
        fi
    fi
}

# 检测 QEMU 支持的加速器
check_qemu_accel() {
    local arch="$1"
    local qemu_bin="qemu-system-${arch}"

    if ! command -v "$qemu_bin" >/dev/null 2>&1; then
        echo "qemu-not-found"
        return
    fi

    local accel_list
    accel_list=$("$qemu_bin" -accel help 2>/dev/null | tail -n +2 | tr '\n' ',' || echo "")
    echo "$accel_list"
}

# 推荐测试架构（基于当前 host 架构和 KVM 可用性）
recommend_arch() {
    local host_arch="$1"
    local kvm_status="$2"

    # 默认使用 x86_64（最常见）
    if [[ "$host_arch" == "x86_64" ]]; then
        echo "x86_64"
    elif [[ "$host_arch" == "aarch64" ]]; then
        echo "aarch64"
    else
        # 其他架构默认用 x86_64（通过 TCG 模拟）
        echo "x86_64"
    fi
}

# 推荐加速模式
recommend_accel() {
    local recommended_arch="$1"
    local host_arch="$2"
    local kvm_status="$3"

    # 如果推荐架构与 host 架构一致且 KVM 可用，使用 KVM
    if [[ "$recommended_arch" == "$host_arch" ]] && [[ "$kvm_status" == "true" ]]; then
        echo "kvm"
    else
        echo "tcg"
    fi
}

# 推荐 QEMU 配置文件
recommend_qemu_config() {
    local arch="$1"
    local accel="$2"
    local vhost="$3"
    local scenario="${4:-vhost}"  # 默认 vhost 场景

    case "$scenario" in
        slirp)
            echo "qemu/slirp-${arch}-${accel}.toml"
            ;;
        tap)
            echo "qemu/tap-${arch}-${accel}.toml"
            ;;
        vhost)
            if [[ "$vhost" == "true" ]]; then
                echo "qemu/vhost-${arch}-${accel}.toml"
            else
                echo "qemu/tap-${arch}-${accel}.toml"
            fi
            ;;
        vhost-smp4)
            if [[ "$vhost" == "true" ]]; then
                echo "qemu/vhost-smp4-${arch}-${accel}.toml"
            else
                echo "qemu/tap-smp4-${arch}-${accel}.toml"
            fi
            ;;
        *)
            echo "qemu/vhost-${arch}-${accel}.toml"
            ;;
    esac
}

# 主检测逻辑
main() {
    local platform host_arch kvm_status vhost_status
    local recommended_arch recommended_accel recommended_config
    local qemu_accel_list

    platform=$(detect_platform)
    host_arch=$(detect_arch)
    kvm_status=$(check_kvm)
    vhost_status=$(check_vhost)
    qemu_accel_list=$(check_qemu_accel "$host_arch")

    recommended_arch=$(recommend_arch "$host_arch" "$kvm_status")
    recommended_accel=$(recommend_accel "$recommended_arch" "$host_arch" "$kvm_status")
    recommended_config=$(recommend_qemu_config "$recommended_arch" "$recommended_accel" "$vhost_status" "vhost")

    if [[ "$OUTPUT_FORMAT" == "json" ]]; then
        cat <<EOF
{
  "platform": "$platform",
  "host_arch": "$host_arch",
  "kvm_available": $([ "$kvm_status" == "true" ] && echo "true" || echo "false"),
  "kvm_status": "$kvm_status",
  "vhost_available": $([ "$vhost_status" == "true" ] && echo "true" || echo "false"),
  "qemu_accel_list": "$qemu_accel_list",
  "recommended_arch": "$recommended_arch",
  "recommended_accel": "$recommended_accel",
  "recommended_config": "$recommended_config"
}
EOF
    else
        cat <<EOF
=== net-bench 环境检测 ===

平台类型:        $platform
Host 架构:       $host_arch
KVM 可用:        $kvm_status
vhost-net 可用:  $vhost_status
QEMU 加速器:     $qemu_accel_list

--- 推荐配置 ---
测试架构:        $recommended_arch
加速模式:        $recommended_accel
QEMU 配置:       $recommended_config

EOF

        # 输出警告和建议
        if [[ "$kvm_status" == "no-permission" ]]; then
            echo "⚠️  /dev/kvm 存在但无权限，运行: sudo chmod 666 /dev/kvm"
        elif [[ "$kvm_status" == "false" ]]; then
            if [[ "$platform" == "wsl2" ]]; then
                echo "⚠️  KVM 不可用（WSL2 需要嵌套虚拟化）"
                echo "    在 Windows 上配置 %USERPROFILE%\\.wslconfig:"
                echo "    [wsl2]"
                echo "    nestedVirtualization=true"
                echo "    然后运行: wsl --shutdown"
            else
                echo "⚠️  KVM 不可用，检查 CPU 虚拟化支持和内核模块"
            fi
        fi

        if [[ "$vhost_status" == "false" ]]; then
            echo "⚠️  vhost-net 不可用，性能会显著降低"
            echo "    尝试: sudo modprobe vhost_net"
        fi

        if [[ "$recommended_accel" == "tcg" ]]; then
            echo "⚠️  将使用 TCG 软件模拟（性能数据仅供功能验证）"
        fi
    fi
}

main
