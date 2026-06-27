#!/usr/bin/env bash
# env/setup-common.sh - 通用网络环境配置（br0/tap0/iperf3/dhcp）
#
# 负责：
#   1. 创建 bridge (br0) 和 TAP (tap0)
#   2. 启动 iperf3 服务器
#   3. 启动 DHCP 服务器（dnsmasq）
#   4. 记录所有创建的资源到状态文件
#   5. 规避已有服务和资源

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE_FILE="${SCRIPT_DIR}/../.bench-state.json"

BRIDGE="${NET_BENCH_BRIDGE:-br0}"
BRIDGE_IP="${NET_BENCH_BRIDGE_IP:-192.168.100.1/24}"
TAP_DEVICE="${NET_BENCH_TAP:-tap0}"
IPERF3_PORT="${NET_BENCH_IPERF3_PORT:-5201}"
DHCP_RANGE="${NET_BENCH_DHCP_RANGE:-192.168.100.10,192.168.100.50,12h}"

# 颜色输出
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
die() { error "$*"; exit 1; }

# 检查 root 权限
check_root() {
    if [[ $EUID -ne 0 ]]; then
        die "需要 root 权限（使用 sudo）"
    fi
}

# 设置设备权限
setup_device_permissions() {
    # KVM 权限
    if [[ -e /dev/kvm ]]; then
        chmod 666 /dev/kvm 2>/dev/null || warn "无法设置 /dev/kvm 权限"
    fi
    
    # vhost-net 权限
    if [[ -e /dev/vhost-net ]]; then
        chmod 666 /dev/vhost-net 2>/dev/null || warn "无法设置 /dev/vhost-net 权限"
    fi
}

# 初始化状态文件
init_state() {
    if [[ ! -f "$STATE_FILE" ]]; then
        cat > "$STATE_FILE" <<EOF
{
  "timestamp": "$(date -Iseconds)",
  "created_resources": [],
  "processes": []
}
EOF
    fi
}

# 记录创建的资源
record_resource() {
    local type="$1"
    local name="$2"
    local details="${3:-}"
    
    local tmp_file=$(mktemp)
    jq ".created_resources += [{\"type\": \"$type\", \"name\": \"$name\", \"details\": \"$details\"}]" "$STATE_FILE" > "$tmp_file"
    mv "$tmp_file" "$STATE_FILE"
}

# 记录启动的进程
record_process() {
    local pid="$1"
    local cmd="$2"
    
    local tmp_file=$(mktemp)
    jq ".processes += [{\"pid\": $pid, \"cmd\": \"$cmd\"}]" "$STATE_FILE" > "$tmp_file"
    mv "$tmp_file" "$STATE_FILE"
}

# 检查端口是否被占用
check_port() {
    local port="$1"
    if ss -tuln | grep -q ":${port} "; then
        return 0  # 端口被占用
    else
        return 1  # 端口空闲
    fi
}

# 检查 bridge 是否存在
bridge_exists() {
    ip link show "$BRIDGE" >/dev/null 2>&1
}

# 检查 TAP 是否存在
tap_exists() {
    ip link show "$TAP_DEVICE" >/dev/null 2>&1
}

# 创建 bridge
setup_bridge() {
    if bridge_exists; then
        warn "Bridge $BRIDGE 已存在，跳过创建"
        return 0
    fi
    
    info "创建 bridge $BRIDGE"
    ip link add "$BRIDGE" type bridge || die "创建 bridge 失败"
    ip link set "$BRIDGE" up || die "启动 bridge 失败"
    ip addr add "$BRIDGE_IP" dev "$BRIDGE" || die "配置 bridge IP 失败"
    
    record_resource "bridge" "$BRIDGE" "$BRIDGE_IP"
    info "Bridge $BRIDGE 创建成功: $BRIDGE_IP"
}

# 创建 TAP 设备
setup_tap() {
    if tap_exists; then
        warn "TAP 设备 $TAP_DEVICE 已存在，跳过创建"
        # 确保 TAP 挂载到 bridge
        if ! brctl show "$BRIDGE" | grep -q "$TAP_DEVICE"; then
            info "将 $TAP_DEVICE 挂载到 $BRIDGE"
            ip link set "$TAP_DEVICE" master "$BRIDGE"
        fi
        return 0
    fi
    
    info "创建 TAP 设备 $TAP_DEVICE"
    ip tuntap add mode tap "$TAP_DEVICE" || die "创建 TAP 设备失败"
    ip link set "$TAP_DEVICE" up || die "启动 TAP 设备失败"
    ip link set "$TAP_DEVICE" master "$BRIDGE" || die "挂载 TAP 到 bridge 失败"
    
    record_resource "tap" "$TAP_DEVICE" "master=$BRIDGE"
    info "TAP 设备 $TAP_DEVICE 创建成功"
}

# 启动 iperf3 服务器
setup_iperf3() {
    local bridge_ip_only="${BRIDGE_IP%%/*}"  # 去掉 CIDR
    
    # 检查端口是否被占用
    if check_port "$IPERF3_PORT"; then
        warn "端口 $IPERF3_PORT 已被占用"
        
        # 检查是否是我们启动的 iperf3
        local existing_pid
        existing_pid=$(pgrep -f "iperf3.*-s.*${bridge_ip_only}" || true)
        
        if [[ -n "$existing_pid" ]]; then
            info "iperf3 服务器已在运行 (PID: $existing_pid)，跳过启动"
            return 0
        else
            warn "端口被其他进程占用，请手动处理"
            return 1
        fi
    fi
    
    info "启动 iperf3 服务器: $bridge_ip_only:$IPERF3_PORT"
    iperf3 -s -B "$bridge_ip_only" -p "$IPERF3_PORT" -D || die "启动 iperf3 失败"
    
    sleep 1
    local iperf3_pid
    iperf3_pid=$(pgrep -f "iperf3.*-s.*${bridge_ip_only}" || true)
    
    if [[ -n "$iperf3_pid" ]]; then
        record_process "$iperf3_pid" "iperf3 -s -B $bridge_ip_only -p $IPERF3_PORT"
        info "iperf3 服务器启动成功 (PID: $iperf3_pid)"
    else
        error "iperf3 启动失败，未找到进程"
        return 1
    fi
}

# 启动 DHCP 服务器（dnsmasq）
setup_dhcp() {
    # 检查 dnsmasq 是否已运行在 bridge 上
    if pgrep -f "dnsmasq.*--interface=${BRIDGE}" >/dev/null; then
        info "dnsmasq 已在 $BRIDGE 上运行，跳过启动"
        return 0
    fi
    
    # 检查 dnsmasq 命令是否存在
    if ! command -v dnsmasq >/dev/null 2>&1; then
        warn "dnsmasq 未安装，跳过 DHCP 服务器配置"
        warn "Guest 需要手动配置静态 IP，或安装: sudo apt-get install -y dnsmasq"
        return 0
    fi
    
    info "启动 DHCP 服务器 (dnsmasq): $DHCP_RANGE"
    dnsmasq \
        --interface="$BRIDGE" \
        --bind-interfaces \
        --dhcp-range="$DHCP_RANGE" \
        --port=0 \
        --no-daemon &
    
    local dnsmasq_pid=$!
    sleep 1
    
    if kill -0 "$dnsmasq_pid" 2>/dev/null; then
        record_process "$dnsmasq_pid" "dnsmasq --interface=$BRIDGE --dhcp-range=$DHCP_RANGE"
        info "DHCP 服务器启动成功 (PID: $dnsmasq_pid)"
        disown  # 让进程在后台继续运行
    else
        error "dnsmasq 启动失败"
        return 1
    fi
}

# 主函数
main() {
    check_root
    
    info "=== net-bench 通用环境配置 ==="
    
    init_state
    
    # 设置设备权限
    setup_device_permissions
    
    # 配置网络
    setup_bridge
    setup_tap
    
    # 启动服务
    setup_iperf3
    setup_dhcp
    
    # 显示配置摘要
    info ""
    info "=== 配置完成 ==="
    info "Bridge:       $BRIDGE ($BRIDGE_IP)"
    info "TAP:          $TAP_DEVICE"
    info "iperf3:       ${BRIDGE_IP%%/*}:$IPERF3_PORT"
    info "DHCP 范围:    $DHCP_RANGE"
    info ""
    info "验证配置:"
    info "  ip addr show $BRIDGE"
    info "  brctl show $BRIDGE"
    info "  ss -tuln | grep $IPERF3_PORT"
}

main "$@"
