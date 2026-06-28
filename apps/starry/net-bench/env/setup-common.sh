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

# 检查并安装主机依赖（已在 root 下运行）。缺失即尝试 apt-get 安装，
# 非 Debian 系或无网络时给出明确指引并退出。
check_dependencies() {
    local missing=()
    local pkg
    # 命令名 -> apt 包名映射（多数同名）。
    declare -A pkg_of=(
        [iperf3]=iperf3 [ip]=iproute2 [brctl]=bridge-utils
        [jq]=jq [dnsmasq]=dnsmasq
    )
    for cmd in iperf3 ip brctl jq dnsmasq; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("${pkg_of[$cmd]}")
    done
    [[ ${#missing[@]} -eq 0 ]] && return 0

    # 去重
    local uniq
    uniq=$(printf '%s\n' "${missing[@]}" | sort -u | tr '\n' ' ')
    warn "缺少依赖: $uniq"
    if command -v apt-get >/dev/null 2>&1; then
        info "尝试安装: apt-get install -y $uniq"
        # shellcheck disable=SC2086
        if DEBIAN_FRONTEND=noninteractive apt-get install -y $uniq; then
            info "依赖安装完成"
        else
            die "依赖安装失败，请手动安装: $uniq"
        fi
    else
        die "缺少依赖且非 apt 系统，请手动安装: $uniq"
    fi
}

# 设置设备权限
setup_device_permissions() {
    # KVM 权限
    if [[ -e /dev/kvm ]]; then
        chmod 666 /dev/kvm 2>/dev/null || warn "无法设置 /dev/kvm 权限"
    fi
    
    # vhost-net：先尝试加载内核模块，再放开权限。新机器上 vhost_net 常未自动
    # 加载，缺少 modprobe 会导致 /dev/vhost-net 不存在、vhost 场景失败。
    if [[ ! -e /dev/vhost-net ]]; then
        modprobe vhost_net 2>/dev/null || warn "无法加载 vhost_net 模块（vhost 场景将不可用）"
    fi
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

# 启动 DHCP 服务器（dnsmasq）
setup_dhcp() {
    # 检查 dnsmasq 是否已运行在 bridge 上
    if pgrep -f "dnsmasq.*--interface=${BRIDGE}" >/dev/null; then
        info "dnsmasq 已在 $BRIDGE 上运行，跳过启动"
        return 0
    fi
    
    # 检查 dnsmasq 命令是否存在
    if ! command -v dnsmasq >/dev/null 2>&1; then
        error "dnsmasq 未安装，但 TAP/vhost 场景的 guest 只支持 DHCP 获取地址。"
        error "安装后重试: sudo apt-get install -y dnsmasq"
        return 1
    fi
    
    info "启动 DHCP 服务器 (dnsmasq): $DHCP_RANGE"
    # 重定向所有 fd 并 setsid 脱离控制终端，避免后台 daemon 持有调用方管道。
    setsid dnsmasq \
        --keep-in-foreground \
        --interface="$BRIDGE" \
        --bind-interfaces \
        --dhcp-range="$DHCP_RANGE" \
        --port=0 </dev/null >/dev/null 2>&1 &
    
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
    
    # 新机器上先补齐主机依赖（iperf3/ip/brctl/jq/dnsmasq）。
    check_dependencies
    
    init_state
    
    # 设置设备权限（含 vhost_net 模块加载）
    setup_device_permissions
    
    # 配置网络
    setup_bridge
    setup_tap
    
    # 启动服务
    # 注意：不在此启动 iperf3 服务端。iperf3 的生命周期由测试入口
    # （run.sh / run-with-perf.sh 的 nb_start_iperf3/nb_stop_iperf3）自管，
    # 这里若启动会与之争用端口 5201（端到端测试已验证此冲突）。
    setup_dhcp
    
    # 显示配置摘要
    info ""
    info "=== 配置完成 ==="
    info "Bridge:       $BRIDGE ($BRIDGE_IP)"
    info "TAP:          $TAP_DEVICE"
    info "DHCP 范围:    $DHCP_RANGE"
    info "iperf3:       由测试入口自管（run.sh），未在此启动"
    info ""
    info "验证配置:"
    info "  ip addr show $BRIDGE"
    info "  brctl show $BRIDGE"
    info "  ss -ulnp | grep :67   # DHCP 服务端"
    info ""
    info "运行测试:"
    info "  bash apps/starry/net-bench/run.sh --scenario vhost --arch x86_64"
}

main "$@"
