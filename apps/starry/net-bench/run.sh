#!/usr/bin/env bash
# apps/starry/net-bench/run.sh — StarryOS 网络性能测试的唯一严肃入口
#
# 设计：本入口"参数明确"——架构、场景、加速器都显式指定（或用明确默认值），
# 不做隐式环境探测。智能/自动检测属于实验性能力，单独放在 bin/bench，
# 默认不参与严肃测试流程（见 README "智能入口（实验性）"一节）。
#
# 用法:
#   bash apps/starry/net-bench/run.sh [options]
#
# options:
#   --scenario S   slirp|tap|vhost|vhost-smp4|tap-smp4（默认 vhost）
#   --arch A       aarch64|x86_64（默认 aarch64）
#   --accel A      kvm|tcg（默认：同架构且 /dev/kvm 可用时 kvm，否则 tcg）
#   --repeat N     每个场景重启 QEMU 跑 N 次，汇总跨启动方差（默认 1）
#   --no-summary   跳过 summarize.py 汇总
#   -h, --help     显示帮助
#
# 兼容旧用法：前两个位置参数仍按 [arch] [scenario] 解析。
#
# 每次 QEMU 启动内部，guest 脚本跑 warmup + ITERS 次迭代（见 net-bench-common.sh），
# 单次 --repeat 已给出 within-boot 的 mean/stddev；--repeat>1 额外覆盖 cross-boot 方差。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=core/lib.sh
. "$SCRIPT_DIR/core/lib.sh"

ARCH="aarch64"
SCENARIO="vhost"
ACCEL=""
REPEAT=1
DO_SUMMARY=true

usage() {
    cat >&2 <<EOF
usage: bash apps/starry/net-bench/run.sh [options]

options:
  --scenario S   slirp|tap|vhost|vhost-smp4|tap-smp4（默认 vhost）
  --arch A       aarch64|x86_64（默认 aarch64）
  --accel A      kvm|tcg（默认：同架构且 /dev/kvm 可用时 kvm，否则 tcg）
  --repeat N     每场景重启 QEMU 跑 N 次并汇总（默认 1）
  --no-summary   跳过自动汇总
  -h, --help     显示帮助

scenario 说明:
  slirp       QEMU usermode networking（仅功能冒烟，性能数据无意义）
  tap         TAP 直连（无 vhost，功能/趋势兜底）
  vhost       TAP + vhost-net（主力性能拓扑）
  vhost-smp4  TAP + vhost-net, smp=4（多核扩展）
  tap-smp4    TAP, smp=4（vhost 不可用时的多核兜底）

TAP/vhost 前置配置:
  sudo bash apps/starry/net-bench/bin/setup
EOF
}

# 解析参数：支持显式选项，并保留 [arch] [scenario] 位置参数向后兼容。
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --scenario) SCENARIO="${2:-}"; shift 2 ;;
        --scenario=*) SCENARIO="${1#*=}"; shift ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --arch=*) ARCH="${1#*=}"; shift ;;
        --accel) ACCEL="${2:-}"; shift 2 ;;
        --accel=*) ACCEL="${1#*=}"; shift ;;
        --repeat) REPEAT="${2:-}"; shift 2 ;;
        --repeat=*) REPEAT="${1#*=}"; shift ;;
        --no-summary) DO_SUMMARY=false; shift ;;
        -h|--help|help) usage; exit 0 ;;
        -*) nb_error "未知选项: $1"; usage; exit 1 ;;
        *) positional+=("$1"); shift ;;
    esac
done
[[ ${#positional[@]} -ge 1 ]] && ARCH="${positional[0]}"
[[ ${#positional[@]} -ge 2 ]] && SCENARIO="${positional[1]}"

[[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || nb_die "--repeat 需要正整数"
nb_validate_arch "$ARCH" || exit 1
nb_validate_scenario "$SCENARIO" || exit 1
[[ -z "$ACCEL" ]] && ACCEL="$(nb_default_accel "$ARCH")"
[[ "$ACCEL" == "kvm" || "$ACCEL" == "tcg" ]] || nb_die "--accel 仅支持 kvm|tcg"

nb_require_cmd iperf3 "apt install iperf3"
nb_require_cmd qemu-system-"$ARCH"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$NB_RESULTS_DIR"

# run_one — 跑单个场景的全部 repeat，并汇总。
run_one() {
    local scenario="$1"
    local accel="$ACCEL"
    local qemu_config; qemu_config="$(nb_qemu_config "$scenario" "$ARCH" "$accel")"

    [[ -f "$qemu_config" ]] || nb_die "QEMU 配置不存在: $qemu_config"
    nb_check_scenario_prereq "$scenario" "$accel"

    local bind_addr=""
    nb_scenario_needs_tap "$scenario" && bind_addr="$NB_TAP_HOST_IP"

    nb_write_fingerprint \
        "$NB_RESULTS_DIR/fingerprint-$ARCH-$scenario-$TIMESTAMP.txt" \
        "$ARCH" "$scenario" "$accel" "$REPEAT"

    nb_info "场景=$scenario 架构=$ARCH 加速=$accel 配置=$qemu_config"

    local run_logs=() rep
    for ((rep = 1; rep <= REPEAT; rep++)); do
        local result_file="$NB_RESULTS_DIR/starry-$ARCH-$scenario-$TIMESTAMP-r${rep}.txt"
        local server_log="$NB_RESULTS_DIR/iperf3-server-$ARCH-$scenario-$TIMESTAMP-r${rep}.log"

        nb_start_iperf3 "$bind_addr" "$server_log"

        nb_section "运行 StarryOS net-bench ($ARCH, $scenario, repeat $rep/$REPEAT)"
        # guest 走 DHCP 获取地址（SLIRP 由 QEMU usermode 应答；TAP/vhost 由 host
        # bridge 上的 DHCP 服务应答，见 nb_check_tap）。无需注入 AX_* 环境变量。
        (cd "$NB_WORKSPACE" && \
            cargo xtask starry app qemu --test-case net-bench --arch "$ARCH" \
                --qemu-config "$qemu_config") 2>&1 | tee "$result_file"

        nb_stop_iperf3
        run_logs+=("$result_file")
        nb_info "结果已保存: $result_file"
    done

    if [[ "$DO_SUMMARY" == "true" ]]; then
        nb_summarize \
            "$NB_RESULTS_DIR/summary-$ARCH-$scenario-$TIMESTAMP.txt" \
            "${run_logs[@]}"
    fi
}

run_one "$SCENARIO"
