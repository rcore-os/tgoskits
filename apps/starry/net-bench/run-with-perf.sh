#!/usr/bin/env bash
# apps/starry/net-bench/run-with-perf.sh — 集成 perf stat 采集 cycles/instructions
#
# ⚠️  DEPRECATED: 请使用 `run.sh --with-perf` 代替。
#    本脚本将继续保留以支持额外的 LLC-load-misses 计数器，但新的开发应
#    优先使用 `run.sh --with-perf`（该入口将 perf 输出传给 summarize.py
#    统一渲染 CPU Efficiency 章节，并提供 IPC 和 cache-miss-rate）。
#
# 用法: bash run-with-perf.sh [--arch A] [--scenario S] [--accel A]
#
# 目标：在运行 net-bench 的同时，用 perf stat 采集 CPU 性能计数器，
# 计算 cycles/byte 和 cycles/packet（methodology §1 核心 KPI）。
#
# 说明：这是显式入口 run.sh 的 perf 包裹变体，复用 core/lib.sh 公共流程，
# 保证 iperf3 生命周期、前置检查、配置矩阵、汇总与 run.sh 完全一致。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=core/lib.sh
. "$SCRIPT_DIR/core/lib.sh"

ARCH="aarch64"
SCENARIO="vhost"
ACCEL=""

usage() {
    cat >&2 <<EOF
usage: bash run-with-perf.sh [--arch A] [--scenario S] [--accel A]

目的：在 net-bench 测试期间采集 perf stat 数据，计算 cycles/byte 和 cycles/packet。
对齐 methodology §1 "CPU 效率" 维度的核心 KPI 要求。

options:
  --arch A       aarch64|x86_64（默认 aarch64）
  --scenario S   slirp|tap|vhost|vhost-smp4|tap-smp4（默认 vhost）
  --accel A      kvm|tcg（默认：同架构且 KVM 可用时 kvm）

输出:
  - results/perf-stat-<arch>-<scenario>-<timestamp>.txt
  - results/summary-<arch>-<scenario>-<timestamp>.txt
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="${2:-}"; shift 2 ;;
        --scenario) SCENARIO="${2:-}"; shift 2 ;;
        --accel) ACCEL="${2:-}"; shift 2 ;;
        -h|--help|help) usage; exit 0 ;;
        *) nb_error "未知选项: $1"; usage; exit 1 ;;
    esac
done

nb_validate_arch "$ARCH" || exit 1
nb_validate_scenario "$SCENARIO" || exit 1
[[ -z "$ACCEL" ]] && ACCEL="$(nb_default_accel "$ARCH")"

nb_require_cmd perf "apt-get install linux-tools-generic"
nb_require_cmd iperf3
nb_require_cmd qemu-system-"$ARCH"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$NB_RESULTS_DIR"

QEMU_CONFIG="$(nb_qemu_config "$SCENARIO" "$ARCH" "$ACCEL")"
[[ -f "$QEMU_CONFIG" ]] || nb_die "QEMU 配置不存在: $QEMU_CONFIG"
nb_check_scenario_prereq "$SCENARIO" "$ACCEL"

BIND_ADDR=""
nb_scenario_needs_tap "$SCENARIO" && BIND_ADDR="$NB_TAP_HOST_IP"

PERF_OUT="$NB_RESULTS_DIR/perf-stat-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
RESULT_FILE="$NB_RESULTS_DIR/starry-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
SERVER_LOG="$NB_RESULTS_DIR/iperf3-server-${ARCH}-${SCENARIO}-${TIMESTAMP}.log"

nb_section "net-bench + perf stat (arch=$ARCH, scenario=$SCENARIO, accel=$ACCEL)"
nb_start_iperf3 "$BIND_ADDR" "$SERVER_LOG"
trap 'nb_stop_iperf3' EXIT

nb_info "perf 输出: $PERF_OUT"
# guest 走 DHCP 获取地址，无需注入 AX_* 环境变量（见 lib.sh nb_guest_env_vars）。
(cd "$NB_WORKSPACE" && \
    perf stat -o "$PERF_OUT" \
        -e cycles,instructions,cache-references,cache-misses,LLC-load-misses \
        -- cargo xtask starry app qemu --test-case net-bench --arch "$ARCH" \
            --qemu-config "$QEMU_CONFIG") 2>&1 | tee "$RESULT_FILE"

nb_stop_iperf3
trap - EXIT

echo ""
nb_section "perf stat 结果"
cat "$PERF_OUT"

# 提取 cycles / instructions，给出 IPC（cycles/byte 需结合 summary 总字节手算）。
CYCLES=$(grep -E '^\s*[0-9,]+\s+cycles' "$PERF_OUT" | awk '{gsub(/,/,"",$1); print $1}')
INSTRUCTIONS=$(grep -E '^\s*[0-9,]+\s+instructions' "$PERF_OUT" | awk '{gsub(/,/,"",$1); print $1}')
if [[ -n "$CYCLES" && -n "$INSTRUCTIONS" ]] && command -v bc >/dev/null 2>&1; then
    IPC=$(echo "scale=3; $INSTRUCTIONS / $CYCLES" | bc -l)
    echo ""
    nb_info "Total cycles: $CYCLES"
    nb_info "Total instructions: $INSTRUCTIONS"
    nb_info "IPC: $IPC"
    nb_info "cycles/byte = cycles / (summary 中 throughput*时长 推算的总字节)"
else
    nb_warn "未能从 perf 输出提取 cycles/instructions（或缺少 bc）"
fi

echo ""
nb_summarize "$NB_RESULTS_DIR/summary-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt" "$RESULT_FILE"

echo ""
nb_section "完成"
nb_info "测试汇总: $NB_RESULTS_DIR/summary-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
nb_info "perf stat: $PERF_OUT"
