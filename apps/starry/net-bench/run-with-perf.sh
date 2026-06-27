#!/usr/bin/env bash
# apps/starry/net-bench/run-with-perf.sh — 集成 perf stat 采集 cycles/instructions
#
# 用法: bash run-with-perf.sh [arch] [scenario]
#
# 目标：在运行 net-bench 的同时，使用 perf stat 采集 CPU 性能计数器，
# 计算 cycles/byte 和 cycles/packet（methodology §1 核心 KPI）。

set -euo pipefail

ARCH="${1:-aarch64}"
SCENARIO="${2:-vhost}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"

usage() {
    cat >&2 <<EOF
usage: bash run-with-perf.sh [arch] [scenario]

目的：在 net-bench 测试期间采集 perf stat 数据，计算 cycles/byte 和 cycles/packet。

对齐 methodology §1 "CPU 效率" 维度的核心 KPI 要求。

输出:
  - results/perf-stat-<arch>-<scenario>-<timestamp>.txt
  - results/summary-<arch>-<scenario>-<timestamp>.txt（包含 cycles 分析）
EOF
}

if [[ "$ARCH" == "help" || "$SCENARIO" == "help" ]]; then
    usage
    exit 0
fi

echo "=== Running net-bench with perf stat (arch=$ARCH, scenario=$SCENARIO) ==="

# 检查 perf 是否可用
if ! command -v perf &>/dev/null; then
    echo "error: perf not found. Install with: sudo apt-get install linux-tools-generic" >&2
    exit 1
fi

# 启动 iperf3 server
TAP_HOST_IP="${TAP_HOST_IP:-192.168.100.1}"
echo "=== Starting iperf3 server on $TAP_HOST_IP:5201 ==="
iperf3 -s -p 5201 -B "$TAP_HOST_IP" &
IPERF_PID=$!
trap "kill $IPERF_PID 2>/dev/null || true" EXIT

sleep 2

# 准备 perf 输出文件
PERF_OUT="$RESULTS_DIR/perf-stat-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"

echo "=== Starting QEMU with perf stat ==="
echo "perf output: $PERF_OUT"

# 运行 cargo xtask 并用 perf stat 包裹整个进程树
# -e cycles,instructions,cache-references,cache-misses,branches,branch-misses
# -d: detailed (L1/LLC cache)
cd "$SCRIPT_DIR/../../.."

perf stat -o "$PERF_OUT" \
    -e cycles,instructions,cache-references,cache-misses,LLC-load-misses \
    -- \
    cargo xtask starry qemu \
        --package net-bench \
        --arch "$ARCH" \
        --toml "apps/starry/net-bench/qemu-${ARCH}-${SCENARIO}.toml" \
        2>&1 | tee "$RESULTS_DIR/starry-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"

# 停止 iperf3 server
kill $IPERF_PID 2>/dev/null || true
trap - EXIT

echo ""
echo "=== perf stat results ==="
cat "$PERF_OUT"

# 解析 perf 输出并计算 cycles/byte
echo ""
echo "=== Analyzing cycles/byte and cycles/packet ==="

# 从 perf 输出提取 cycles 和 instructions
CYCLES=$(grep -E '^\s*[0-9,]+\s+cycles' "$PERF_OUT" | awk '{gsub(/,/,"",$1); print $1}')
INSTRUCTIONS=$(grep -E '^\s*[0-9,]+\s+instructions' "$PERF_OUT" | awk '{gsub(/,/,"",$1); print $1}')

if [[ -n "$CYCLES" && -n "$INSTRUCTIONS" ]]; then
    IPC=$(echo "scale=3; $INSTRUCTIONS / $CYCLES" | bc -l)
    echo "Total cycles: $CYCLES"
    echo "Total instructions: $INSTRUCTIONS"
    echo "IPC: $IPC"
    
    # 从 iperf3 结果提取总字节数（需要解析 summarize.py 输出）
    # 这里先记录 cycles，后续可以手动或自动化计算 cycles/byte
    echo ""
    echo "Note: To calculate cycles/byte, divide total cycles by total bytes from iperf3 summary."
    echo "      To calculate cycles/packet, divide total cycles by total packets."
else
    echo "warning: Could not extract cycles/instructions from perf output" >&2
fi

echo ""
echo "=== Summarizing test results ==="
python3 "$SCRIPT_DIR/summarize.py" "$RESULTS_DIR/starry-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt" \
    > "$RESULTS_DIR/summary-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"

cat "$RESULTS_DIR/summary-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"

echo ""
echo "=== Complete ==="
echo "Results:"
echo "  - Test summary: $RESULTS_DIR/summary-${ARCH}-${SCENARIO}-${TIMESTAMP}.txt"
echo "  - perf stat: $PERF_OUT"
