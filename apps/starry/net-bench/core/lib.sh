#!/usr/bin/env bash
# core/lib.sh — net-bench 主机侧公共流程封装（host-side shared library）
#
# 目标：把"配置常量、配置文件解析、iperf3 服务端生命周期、环境前置检查、
# 环境指纹、结果汇总"等公用环节集中封装，使各入口（显式入口 run.sh、实验性
# 智能入口 bin/bench、Linux 基线 run-linux-baseline.sh）路径清晰、行为一致。
#
# 用法：调用方先 `source core/lib.sh`，再使用 nb_* 函数。本文件不自带 `set`，
# 由调用方控制 shell 选项；所有函数对未配置项给出明确错误并返回非零。
#
# 设计原则：
#   - 严肃测试入口"入口 + 参数明确"，不依赖隐式自动检测。
#   - 拓扑/网段/端口集中为常量，避免散落硬编码（评估文档"硬编码分散"问题）。
#   - SLIRP 仅功能冒烟，性能拓扑用 TAP / vhost-net。

# ---- 公共常量（集中管理，消除散落硬编码） --------------------------------

# TAP / vhost 拓扑网段（host 端在 br0 上，guest 静态地址 + 网关指向 host）。
NB_TAP_HOST_IP="${NB_TAP_HOST_IP:-192.168.100.1}"
NB_TAP_GUEST_IP="${NB_TAP_GUEST_IP:-192.168.100.2}"
NB_TAP_PREFIX_LEN="${NB_TAP_PREFIX_LEN:-24}"
NB_TAP_IFACE="${NB_TAP_IFACE:-tap0}"
NB_BRIDGE="${NB_BRIDGE:-br0}"

# SLIRP（QEMU usermode）拓扑下，guest 通过网关 10.0.2.2 访问 host 服务。
NB_SLIRP_HOST_IP="${NB_SLIRP_HOST_IP:-10.0.2.2}"

# iperf3 服务端端口（guest 侧 net-bench-common.sh 默认同值）。
NB_IPERF3_PORT="${NB_IPERF3_PORT:-5201}"

# ---- 路径推导 ------------------------------------------------------------

# net-bench 根目录（core/ 的上一级）。
NB_BENCH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NB_WORKSPACE="$(cd "$NB_BENCH_ROOT/../../.." && pwd)"
NB_QEMU_DIR="$NB_BENCH_ROOT/qemu"
NB_CORE_DIR="$NB_BENCH_ROOT/core"
NB_ENV_DIR="$NB_BENCH_ROOT/env"
NB_RESULTS_DIR="$NB_BENCH_ROOT/results"
NB_SUMMARIZER="$NB_CORE_DIR/summarize.py"

# ---- 日志输出 ------------------------------------------------------------

if [[ -t 2 ]]; then
    NB_C_GREEN=$'\033[0;32m'; NB_C_YELLOW=$'\033[1;33m'
    NB_C_RED=$'\033[0;31m'; NB_C_BLUE=$'\033[0;34m'; NB_C_NC=$'\033[0m'
else
    NB_C_GREEN=""; NB_C_YELLOW=""; NB_C_RED=""; NB_C_BLUE=""; NB_C_NC=""
fi

nb_info()    { echo "${NB_C_GREEN}[INFO]${NB_C_NC} $*"; }
nb_warn()    { echo "${NB_C_YELLOW}[WARN]${NB_C_NC} $*" >&2; }
nb_error()   { echo "${NB_C_RED}[ERROR]${NB_C_NC} $*" >&2; }
nb_section() { echo "${NB_C_BLUE}===== $* =====${NB_C_NC}"; }
nb_die()     { nb_error "$*"; exit 1; }

# ---- 场景 / 架构 / 加速器 -------------------------------------------------

# 支持的场景与架构（严肃入口需"参数明确"，集中校验）。
NB_SCENARIOS="slirp tap vhost vhost-smp4 tap-smp4"
NB_ARCHES="aarch64 x86_64"

# nb_validate_scenario <scenario>
nb_validate_scenario() {
    case " $NB_SCENARIOS " in
        *" $1 "*) return 0 ;;
        *) nb_error "未知场景: $1（有效: $NB_SCENARIOS）"; return 1 ;;
    esac
}

# nb_validate_arch <arch>
nb_validate_arch() {
    case " $NB_ARCHES " in
        *" $1 "*) return 0 ;;
        *) nb_error "未知架构: $1（有效: $NB_ARCHES）"; return 1 ;;
    esac
}

# nb_host_arch — 返回 uname -m 归一化后的 host 架构。
nb_host_arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) uname -m ;;
    esac
}

# nb_default_accel <arch> — 同架构且 /dev/kvm 可用时用 kvm，否则 tcg。
nb_default_accel() {
    local arch="$1"
    if [[ "$arch" == "$(nb_host_arch)" && -e /dev/kvm && -r /dev/kvm && -w /dev/kvm ]]; then
        echo "kvm"
    else
        echo "tcg"
    fi
}

# nb_qemu_config <scenario> <arch> <accel> — 返回统一矩阵中的配置文件路径。
# vhost 与 tap 共享同一加速维度；smp4 变体追加 -smp4 后缀。
nb_qemu_config() {
    local scenario="$1" arch="$2" accel="$3"
    echo "$NB_QEMU_DIR/${scenario}-${arch}-${accel}.toml"
}

# nb_scenario_host_ip <scenario> — 该场景下 guest 访问 host 的地址。
nb_scenario_host_ip() {
    case "$1" in
        slirp) echo "$NB_SLIRP_HOST_IP" ;;
        *)     echo "$NB_TAP_HOST_IP" ;;
    esac
}

# nb_scenario_needs_tap <scenario> — TAP/vhost 场景需要 host 侧 bridge+tap。
nb_scenario_needs_tap() {
    [[ "$1" != "slirp" ]]
}

# nb_scenario_needs_vhost <scenario> — vhost 场景需要 /dev/vhost-net。
nb_scenario_needs_vhost() {
    case "$1" in vhost|vhost-smp4) return 0 ;; *) return 1 ;; esac
}

# nb_guest_env_vars <scenario> — 打印该场景下需要传入 guest 的环境变量
# （每行一个 KEY=VALUE）。
#
# 重要：当前 StarryOS/ArceOS 在 `cargo xtask starry app qemu` 路径下的 guest
# 内核只支持 DHCP 获取地址——没有任何 crate 读取 AX_IP/AX_GW/AX_PREFIX_LEN
# （axruntime 的 parse_network_config 恒为 NetworkConfig::default()，
# 见 os/arceos/modules/axruntime/src/devices.rs）。因此本函数不再注入这些
# 无效变量。guest 地址由网络侧的 DHCP 服务决定：
#   - SLIRP：QEMU usermode 内建 DHCP 自动应答；
#   - TAP/vhost：需 host 在 bridge 上运行 DHCP 服务（见 nb_check_tap）。
# 待 guest 支持静态 IP（读 AX_IP 等）后，可在此恢复注入。
nb_guest_env_vars() {
    :  # 目前无需注入任何 guest 环境变量（guest 走 DHCP）。
}

# ---- 前置检查 ------------------------------------------------------------

nb_require_cmd() {
    command -v "$1" >/dev/null 2>&1 || nb_die "缺少命令: $1${2:+（$2）}"
}

nb_check_kvm() {
    [[ -e /dev/kvm ]] || nb_die "/dev/kvm 不存在（KVM 加速所需）。WSL2 需启用嵌套虚拟化。"
}

nb_check_vhost() {
    [[ -e /dev/vhost-net ]] || nb_die "/dev/vhost-net 不存在（vhost 场景所需）。尝试: sudo modprobe vhost_net"
}

# nb_check_tap — 确认 TAP/bridge 已配置、host 侧持有目标地址，且有 DHCP 服务
# 应答（guest 走 DHCP，缺 DHCP 服务时 guest 会 "DHCP bootstrap timed out"）。
nb_check_tap() {
    nb_require_cmd ip
    ip link show "$NB_TAP_IFACE" >/dev/null 2>&1 || nb_die \
        "TAP 接口 $NB_TAP_IFACE 不存在。先配置: sudo bash $NB_BENCH_ROOT/bin/setup"
    if ip link show "$NB_BRIDGE" >/dev/null 2>&1; then
        ip -4 addr show dev "$NB_BRIDGE" | grep -q "$NB_TAP_HOST_IP/$NB_TAP_PREFIX_LEN" || nb_die \
            "Bridge $NB_BRIDGE 缺少地址 $NB_TAP_HOST_IP/$NB_TAP_PREFIX_LEN。重新配置: sudo bash $NB_BENCH_ROOT/bin/setup"
    else
        ip -4 addr show dev "$NB_TAP_IFACE" | grep -q "$NB_TAP_HOST_IP/$NB_TAP_PREFIX_LEN" || nb_die \
            "TAP 接口 $NB_TAP_IFACE 缺少地址 $NB_TAP_HOST_IP/$NB_TAP_PREFIX_LEN。配置: sudo bash $NB_BENCH_ROOT/bin/setup"
    fi
    nb_check_dhcp_server
}

# nb_check_dhcp_server — 确认 bridge/tap 上有 DHCP 服务监听 :67。guest 内核只
# 支持 DHCP（见 nb_guest_env_vars 说明），缺此服务 guest 取不到 IP 而失败。
nb_check_dhcp_server() {
    command -v ss >/dev/null 2>&1 || return 0  # 无 ss 时跳过软校验
    ss -H -ulnp "sport = :67" 2>/dev/null | grep -q . || nb_die \
        "未检测到 DHCP 服务监听 :67。TAP/vhost 场景的 guest 走 DHCP，需在 $NB_BRIDGE 上启动 DHCP 服务。配置: sudo bash $NB_BENCH_ROOT/bin/setup（含 dnsmasq）"
}

# nb_check_scenario_prereq <scenario> <accel> — 按场景/加速器校验前置条件。
nb_check_scenario_prereq() {
    local scenario="$1" accel="$2"
    [[ "$accel" == "kvm" ]] && nb_check_kvm
    if nb_scenario_needs_vhost "$scenario"; then
        nb_check_kvm
        nb_check_vhost
    fi
    nb_scenario_needs_tap "$scenario" && nb_check_tap
    return 0
}

# ---- iperf3 服务端生命周期 ------------------------------------------------

# 模块级变量：当前 iperf3 服务端 PID（nb_stop_iperf3 使用）。
NB_IPERF3_PID=""

nb_iperf3_port_busy() {
    command -v ss >/dev/null 2>&1 || return 1
    ss -H -tln "sport = :$NB_IPERF3_PORT" 2>/dev/null | grep -q .
}

# nb_start_iperf3 <bind_addr> <log_file>
# bind_addr 为空时监听 0.0.0.0（SLIRP 场景）。设置 NB_IPERF3_PID，并注册
# 进程级 EXIT/INT/TERM 清理，确保即使调用方中途 die/被中断也不残留服务端。
nb_start_iperf3() {
    local bind_addr="$1" log_file="$2"
    if nb_iperf3_port_busy; then
        nb_error "TCP 端口 $NB_IPERF3_PORT 已被占用，先停止已有 iperf3 服务端"
        command -v ss >/dev/null 2>&1 && ss -tlnp "sport = :$NB_IPERF3_PORT" >&2 || true
        return 1
    fi
    if [[ -n "$bind_addr" ]]; then
        nb_info "启动 host iperf3 服务端 $bind_addr:$NB_IPERF3_PORT"
        iperf3 -s -p "$NB_IPERF3_PORT" -B "$bind_addr" > "$log_file" 2>&1 &
    else
        nb_info "启动 host iperf3 服务端 0.0.0.0:$NB_IPERF3_PORT"
        iperf3 -s -p "$NB_IPERF3_PORT" > "$log_file" 2>&1 &
    fi
    NB_IPERF3_PID=$!
    # 进程级兜底清理：RETURN trap 只覆盖正常函数返回，die/中断需 EXIT 兜底。
    trap 'nb_stop_iperf3' EXIT INT TERM
    sleep 1
    kill -0 "$NB_IPERF3_PID" 2>/dev/null || { nb_error "iperf3 服务端启动失败，见 $log_file"; return 1; }
}

# nb_stop_iperf3 — 终止当前 iperf3 服务端（幂等）。
nb_stop_iperf3() {
    [[ -n "$NB_IPERF3_PID" ]] || return 0
    kill "$NB_IPERF3_PID" 2>/dev/null || true
    NB_IPERF3_PID=""
}

# ---- 环境指纹 ------------------------------------------------------------

# nb_write_fingerprint <file> <arch> <scenario> <accel> <repeat>
# 记录可复现性所需的环境指纹（methodology §3.4 / plan §6.3）。
nb_write_fingerprint() {
    local file="$1" arch="$2" scenario="$3" accel="$4" repeat="$5"
    {
        echo "# net-bench environment fingerprint"
        echo "timestamp    : $(date +%Y%m%d-%H%M%S)"
        echo "arch         : $arch"
        echo "scenario     : $scenario"
        echo "accel        : $accel"
        echo "repeat       : $repeat"
        echo "host_uname   : $(uname -a)"
        echo "host_nproc   : $(nproc 2>/dev/null || echo '?')"
        local qemu_bin; qemu_bin="$(command -v "qemu-system-$arch" 2>/dev/null || true)"
        if [[ -n "$qemu_bin" ]]; then
            echo "qemu         : $("$qemu_bin" --version 2>/dev/null | head -1)"
            echo "qemu_accel   : $("$qemu_bin" -accel help 2>/dev/null | tail -n +2 | tr '\n' ' ')"
        fi
        echo "iperf3_host  : $(iperf3 --version 2>/dev/null | head -1)"
        echo "kvm          : $([[ -e /dev/kvm ]] && echo present || echo absent)"
        echo "vhost_net    : $([[ -e /dev/vhost-net ]] && echo present || echo absent)"
        echo "starry_commit: $(git -C "$NB_WORKSPACE" rev-parse --short HEAD 2>/dev/null || echo '?')"
    } > "$file"
    nb_info "环境指纹 -> $file"
    cat "$file"
}

# ---- 结果汇总 ------------------------------------------------------------

# nb_summarize <summary_file> <run_log...> [--perf <perf_file>...]
# 调用 summarize.py 产出 mean/stddev。所有额外参数（含 --perf）透传给 summarize.py。
nb_summarize() {
    local summary_file="$1"; shift
    if ! command -v python3 >/dev/null 2>&1; then
        nb_warn "python3 未找到，跳过自动汇总"
        return 0
    fi
    nb_info "汇总结果 -> $summary_file"
    python3 "$NB_SUMMARIZER" "$@" | tee "$summary_file"
}
