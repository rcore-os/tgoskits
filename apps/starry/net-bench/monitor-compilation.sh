#!/usr/bin/env bash
# monitor-compilation.sh — 监控 Starry 编译进度

LOG_FILE="/tmp/starry-vhost-test.log"
RESULT_DIR="/home/asta/tgoskits/wt-feat-net-enhance/apps/starry/net-bench/results"

echo "=== Monitoring Starry compilation and test ==="
echo "Log file: $LOG_FILE"
echo ""

while true; do
    # 检查编译进程
    if pgrep -f "cargo.*starryos" > /dev/null; then
        echo "[$(date +%H:%M:%S)] Compilation in progress..."

        # 显示最新编译信息
        if [[ -f "$LOG_FILE" ]]; then
            tail -3 "$LOG_FILE" | grep -E "Compiling|Finished" || echo "  (still compiling dependencies...)"
        fi
    elif pgrep -f "qemu-system-aarch64" > /dev/null; then
        echo "[$(date +%H:%M:%S)] QEMU running, test in progress..."

        # 检查是否有测试输出
        if [[ -f "$LOG_FILE" ]]; then
            if grep -q "NET_BENCH_BEGIN" "$LOG_FILE"; then
                echo "  ✅ Test started!"
                tail -5 "$LOG_FILE" | grep -E "NET_BENCH|iperf3"
            fi
        fi
    else
        echo "[$(date +%H:%M:%S)] No active processes found."

        # 检查是否有新结果
        LATEST_RESULT=$(ls -t "$RESULT_DIR"/starry-aarch64-vhost-*.txt 2>/dev/null | head -1)
        if [[ -n "$LATEST_RESULT" ]]; then
            echo ""
            echo "Latest result: $LATEST_RESULT"

            # 检查测试是否完成
            if grep -q "NET_BENCH_PASSED" "$LATEST_RESULT"; then
                echo "  ✅ Test PASSED!"
                exit 0
            elif grep -q "NET_BENCH_FAILED" "$LATEST_RESULT"; then
                echo "  ❌ Test FAILED!"
                exit 1
            else
                echo "  ⏳ Test incomplete or in progress"
            fi
        fi

        echo ""
        echo "Compilation may have completed. Check manually with:"
        echo "  tail -100 $LOG_FILE"
        break
    fi

    echo ""
    sleep 30
done
