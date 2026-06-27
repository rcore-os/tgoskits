#!/usr/bin/env bash
# apps/starry/net-bench/setup-vhost-tap.sh
#
# 一键配置 QEMU+TAP+vhost-net 测试环境（对齐 qemu-benchmark-plan §2）。
#
# 前置条件:
#   - WSL2 + Win11 + 嵌套虚拟化开启（确保 /dev/kvm 存在）
#   - 内核支持 vhost_net（CONFIG_VHOST_NET=y/m）
#   - sudo 权限（创建 bridge、tap、加载内核模块）
#
# 用法:
#   bash apps/starry/net-bench/setup-vhost-tap.sh [setup|check|teardown]
#     setup     创建 br0 + tap0/tap1（默认）
#     check     检查环境前置条件
#     teardown  清理 br0 + tap0/tap1
#
# 拓扑 A（单 guest，默认）:
#   [Starry guest] --tap0--> br0 <-- [WSL2 host iperf3 server]
#   br0: 192.168.100.1/24
#   guest: 192.168.100.2/24
#
# 拓扑 B（双 guest，可选，需手动修改）:
#   [Starry guest] --tap0--> br0 <--tap1-- [Linux guest]
#   br0: 192.168.100.1/24
#   guest0: 192.168.100.2/24, guest1: 192.168.100.3/24

set -euo pipefail

ACTION="${1:-setup}"
BRIDGE="br0"
BRIDGE_IP="192.168.100.1/24"
TAP0="tap0"
TAP1="tap1"  # 可选，拓扑 B 双 guest 时使用

# 颜色输出
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
die() { error "$*"; exit 1; }

check_root() {
    if [[ $EUID -ne 0 ]]; then
        die "This script must be run as root (use sudo)"
    fi
}

check_kvm() {
    if [[ ! -e /dev/kvm ]]; then
        error "/dev/kvm not found"
        cat >&2 <<EOF
WSL2 requires:
  1. Windows 11 (or Win10 21H2+ with KB5020030)
  2. Enable nested virtualization in .wslconfig:
       [wsl2]
       nestedVirtualization=true
  3. Restart WSL: wsl --shutdown
EOF
        return 1
    fi
    if [[ ! -r /dev/kvm ]] || [[ ! -w /dev/kvm ]]; then
        error "/dev/kvm exists but not accessible"
        warn "Try: sudo chmod 666 /dev/kvm"
        return 1
    fi
    info "/dev/kvm present and accessible"
    return 0
}

check_vhost_net() {
    if [[ ! -e /dev/vhost-net ]]; then
        warn "/dev/vhost-net not found, trying to load vhost_net module"
        if ! modprobe vhost_net 2>/dev/null; then
            error "Failed to load vhost_net module"
            cat >&2 <<EOF
Your kernel may not have CONFIG_VHOST_NET enabled.
Check with: zgrep VHOST_NET /proc/config.gz
If absent, you need to recompile the WSL2 kernel or use a distro kernel with vhost_net.
Without vhost-net, performance will be significantly lower (fallback to qemu userspace).
EOF
            return 1
        fi
    fi
    if [[ ! -c /dev/vhost-net ]]; then
        error "/dev/vhost-net is not a character device"
        return 1
    fi
    info "/dev/vhost-net present"
    return 0
}

check_commands() {
    local missing=()
    for cmd in ip brctl iperf3; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            missing+=("$cmd")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        error "Missing commands: ${missing[*]}"
        warn "Install with: sudo apt-get install -y iproute2 bridge-utils iperf3"
        return 1
    fi
    info "Required commands present: ip, brctl, iperf3"
    return 0
}

check_env() {
    info "=== Checking vhost-net environment ==="
    local ok=0
    check_kvm || ok=1
    check_vhost_net || ok=1
    check_commands || ok=1
    
    if [[ $ok -eq 0 ]]; then
        info "Environment check passed ✓"
        return 0
    else
        error "Environment check failed, see errors above"
        return 1
    fi
}

setup_bridge() {
    info "=== Setting up bridge + TAP for vhost-net ==="
    
    # 创建 bridge
    if ip link show "$BRIDGE" >/dev/null 2>&1; then
        warn "Bridge $BRIDGE already exists, skipping creation"
    else
        info "Creating bridge $BRIDGE"
        ip link add "$BRIDGE" type bridge
        ip addr add "$BRIDGE_IP" dev "$BRIDGE"
        ip link set "$BRIDGE" up
    fi
    
    # 创建 tap0（拓扑 A 必需）
    if ip link show "$TAP0" >/dev/null 2>&1; then
        warn "TAP interface $TAP0 already exists, skipping creation"
    else
        info "Creating TAP interface $TAP0"
        ip tuntap add dev "$TAP0" mode tap user "${SUDO_USER:-$USER}"
        ip link set "$TAP0" master "$BRIDGE"
        ip link set "$TAP0" up
    fi
    
    # 可选：创建 tap1（拓扑 B 双 guest）
    # 默认不创建，按需取消注释：
    # if ip link show "$TAP1" >/dev/null 2>&1; then
    #     warn "TAP interface $TAP1 already exists, skipping creation"
    # else
    #     info "Creating TAP interface $TAP1 (topology B)"
    #     ip tuntap add dev "$TAP1" mode tap user "${SUDO_USER:-$USER}"
    #     ip link set "$TAP1" master "$BRIDGE"
    #     ip link set "$TAP1" up
    # fi
    
    info "Bridge and TAP setup complete"
    ip addr show "$BRIDGE"
    brctl show "$BRIDGE"
}

teardown_bridge() {
    info "=== Tearing down bridge + TAP ==="
    
    for iface in "$TAP0" "$TAP1"; do
        if ip link show "$iface" >/dev/null 2>&1; then
            info "Removing $iface"
            ip link set "$iface" down 2>/dev/null || true
            ip link delete "$iface" 2>/dev/null || true
        fi
    done
    
    if ip link show "$BRIDGE" >/dev/null 2>&1; then
        info "Removing bridge $BRIDGE"
        ip link set "$BRIDGE" down 2>/dev/null || true
        ip link delete "$BRIDGE" 2>/dev/null || true
    fi
    
    info "Teardown complete"
}

show_usage() {
    cat <<EOF
usage: sudo bash apps/starry/net-bench/setup-vhost-tap.sh [setup|check|teardown]

commands:
  setup     Create br0 + tap0 for QEMU+vhost-net testing (default)
  check     Verify /dev/kvm, /dev/vhost-net, and required commands
  teardown  Remove br0 + tap0

topology A (single guest, default):
  [Starry guest] --tap0--> br0 <-- [WSL2 host iperf3]
  br0: 192.168.100.1/24
  guest: 192.168.100.2/24 (set via AX_IP/AX_GW env vars)

after setup, run:
  bash apps/starry/net-bench/run.sh aarch64 vhost
EOF
}

case "$ACTION" in
    setup)
        check_root
        check_env || die "Environment check failed, cannot proceed"
        setup_bridge
        info "Setup complete. Next step: bash apps/starry/net-bench/run.sh aarch64 vhost"
        ;;
    check)
        check_env
        ;;
    teardown)
        check_root
        teardown_bridge
        ;;
    help|--help|-h)
        show_usage
        ;;
    *)
        error "Unknown action: $ACTION"
        show_usage
        exit 1
        ;;
esac
