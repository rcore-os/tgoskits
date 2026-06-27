#!/bin/sh
# apps/starry/net-bench/net-bench-netperf.sh — guest 侧 netperf 延迟测试
#
# 用途：补充 iperf3 无法覆盖的延迟测试维度（methodology §2.1）
#   - TCP_RR: TCP 请求-响应延迟（暴露 poll/yield/调度尾延迟）
#   - UDP_RR: UDP 请求-响应延迟
#   - TCP_CRR: TCP 短连接速率（压 listen_table/端口分配锁）
#
# 对齐 methodology §1 六维指标中的"延迟"和"连接速率"维度。

set -e

HOST_IP="${HOST_IP:-10.0.2.2}"
WARMUP_ITER="${WARMUP_ITER:-1}"
MEASURE_ITER="${MEASURE_ITER:-5}"
TEST_DURATION="${TEST_DURATION:-10}"

echo "=== net-bench netperf latency tests ==="
echo "HOST_IP=$HOST_IP, warmup=$WARMUP_ITER, measure=$MEASURE_ITER, duration=${TEST_DURATION}s"

# 等待 netserver 就绪
echo "=== waiting for netserver on $HOST_IP:12865 ==="
retry=0
while [ $retry -lt 15 ]; do
    if nc -z "$HOST_IP" 12865 2>/dev/null; then
        echo "=== netserver ready ==="
        break
    fi
    retry=$((retry + 1))
    sleep 1
done

if [ $retry -eq 15 ]; then
    echo "NET_BENCH_FAILED: netserver unreachable after 15 retries"
    exit 1
fi

# netperf 通用包装器
run_netperf_test() {
    local test_id="$1"
    local netperf_test="$2"
    shift 2
    local extra_args="$*"
    
    # warmup
    echo "NET_BENCH_BEGIN test_id=$test_id boot=1 iter=warmup"
    netperf -H "$HOST_IP" -t "$netperf_test" -l "$TEST_DURATION" $extra_args || echo "warmup: netperf returned non-zero"
    echo "NET_BENCH_END test_id=$test_id boot=1 iter=warmup"
    
    # 测量迭代
    iter=1
    while [ $iter -le "$MEASURE_ITER" ]; do
        echo "NET_BENCH_BEGIN test_id=$test_id boot=1 iter=$iter"
        netperf -H "$HOST_IP" -t "$netperf_test" -l "$TEST_DURATION" $extra_args
        echo "NET_BENCH_END test_id=$test_id boot=1 iter=$iter"
        iter=$((iter + 1))
    done
}

# TCP_RR: TCP 请求-响应延迟（每秒事务数 + 延迟）
# 输出格式: <transactions/s> <mean_latency_us>
echo "=== TCP_RR: TCP request-response latency ==="
run_netperf_test tcp_rr TCP_RR -- -o THROUGHPUT,MEAN_LATENCY,P50_LATENCY,P90_LATENCY,P99_LATENCY

# UDP_RR: UDP 请求-响应延迟
echo "=== UDP_RR: UDP request-response latency ==="
run_netperf_test udp_rr UDP_RR -- -o THROUGHPUT,MEAN_LATENCY,P50_LATENCY,P90_LATENCY,P99_LATENCY

# TCP_CRR: TCP 连接-请求-响应 (含 connect/accept/close 的短连接速率)
echo "=== TCP_CRR: TCP connection rate (short-lived connections) ==="
run_netperf_test tcp_crr TCP_CRR -- -o THROUGHPUT,MEAN_LATENCY

echo "NET_BENCH_PASSED"
