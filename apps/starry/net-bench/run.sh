#!/usr/bin/env bash
# apps/starry/net-bench/run.sh — StarryOS 网络性能测试入口
#
# 用法: bash apps/starry/net-bench/run.sh [arch] [scenario] [--repeat N]
#   arch      当前仅支持 aarch64
#   scenario  默认 slirp，可选：slirp, slirp-smp4, tap, all
#   --repeat  每个场景重启 QEMU 跑 N 次，汇总跨启动方差（默认 1）
#
# 每次 QEMU 启动内部，guest 脚本会跑 warmup + 5 次迭代（见 net-bench-common.sh），
# 因此单次 --repeat 已能给出 within-boot 的 mean/stddev；--repeat>1 额外覆盖
# cross-boot 方差。运行结束后自动调用 summarize.py 产出 per-test mean/stddev。
set -euo pipefail

ARCH="aarch64"
SCENARIO="slirp"
REPEAT=1

# 解析位置参数与 --repeat（保持向后兼容：前两个非选项参数是 arch / scenario）。
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repeat)
            REPEAT="${2:-}"; shift 2
            [[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || { echo "error: --repeat needs a positive integer" >&2; exit 1; }
            ;;
        --repeat=*)
            REPEAT="${1#*=}"; shift
            [[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || { echo "error: --repeat needs a positive integer" >&2; exit 1; }
            ;;
        -h|--help|help)
            positional+=("help"); shift ;;
        *)
            positional+=("$1"); shift ;;
    esac
done
[[ ${#positional[@]} -ge 1 ]] && ARCH="${positional[0]}"
[[ ${#positional[@]} -ge 2 ]] && SCENARIO="${positional[1]}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
SUMMARIZER="$SCRIPT_DIR/summarize.py"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
IPERF3_PORT=5201
TAP_IFACE="${TAP_IFACE:-tap0}"
TAP_HOST_IP="${TAP_HOST_IP:-192.168.100.1}"

mkdir -p "$RESULTS_DIR"

usage() {
    cat >&2 <<EOF
usage: bash apps/starry/net-bench/run.sh [arch] [scenario] [--repeat N]

scenario:
  slirp       QEMU usermode networking, smp=1 (default, smoke only)
  slirp-smp4  QEMU usermode networking, smp=4
  tap         TAP direct networking, requires $TAP_IFACE=$TAP_HOST_IP/24
  all         Run slirp, slirp-smp4, then tap

options:
  --repeat N  Reboot QEMU N times per scenario and aggregate (default 1)
EOF
}

check_arch() {
    if [[ "$ARCH" != "aarch64" ]]; then
        echo "error: apps/starry/net-bench currently only provides aarch64 QEMU configs" >&2
        exit 1
    fi
}

# 记录环境指纹（methodology §3.4 / plan §6.3 要求），写到 results 目录。
write_fingerprint() {
    local file="$1"
    {
        echo "# net-bench environment fingerprint"
        echo "timestamp   : $TIMESTAMP"
        echo "arch        : $ARCH"
        echo "scenario    : $SCENARIO"
        echo "repeat      : $REPEAT"
        echo "host_uname  : $(uname -a)"
        echo "host_nproc  : $(nproc 2>/dev/null || echo '?')"
        local qemu_bin; qemu_bin="$(command -v "qemu-system-$ARCH" 2>/dev/null || true)"
        if [[ -n "$qemu_bin" ]]; then
            echo "qemu        : $("$qemu_bin" --version 2>/dev/null | head -1)"
            echo "qemu_accel  : $("$qemu_bin" -accel help 2>/dev/null | tail -n +2 | tr '\n' ' ')"
        fi
        echo "iperf3_host : $(iperf3 --version 2>/dev/null | head -1)"
        echo "kvm         : $([[ -e /dev/kvm ]] && echo present || echo absent)"
        echo "vhost_net   : $([[ -e /dev/vhost-net ]] && echo present || echo absent)"
        local commit; commit="$(git -C "$WORKSPACE" rev-parse --short HEAD 2>/dev/null || echo '?')"
        echo "starry_commit: $commit"
    } > "$file"
    echo "=== net-bench: environment fingerprint -> $file ==="
    cat "$file"
}

ensure_port_free() {
    if command -v ss >/dev/null 2>&1 && ss -H -tln "sport = :$IPERF3_PORT" | grep -q .; then
        echo "error: TCP port $IPERF3_PORT is already listening on host" >&2
        echo "hint: stop the existing server, or confirm it listens on the expected address before running manually" >&2
        ss -tlnp "sport = :$IPERF3_PORT" >&2 || true
        exit 1
    fi
}

start_iperf3_server() {
    local bind_addr="$1" log_file="$2"
    ensure_port_free
    if [[ -n "$bind_addr" ]]; then
        echo "=== net-bench: starting host iperf3 server on $bind_addr:$IPERF3_PORT ==="
        iperf3 -s -p "$IPERF3_PORT" -B "$bind_addr" > "$log_file" 2>&1 &
    else
        echo "=== net-bench: starting host iperf3 server on 0.0.0.0:$IPERF3_PORT ==="
        iperf3 -s -p "$IPERF3_PORT" > "$log_file" 2>&1 &
    fi
    iperf3_pid=$!
    sleep 1
    if ! kill -0 "$iperf3_pid" 2>/dev/null; then
        echo "error: failed to start iperf3 server, see $log_file" >&2
        exit 1
    fi
}

check_tap() {
    command -v ip >/dev/null 2>&1 || { echo "error: ip command not found" >&2; exit 1; }
    if ! ip link show "$TAP_IFACE" >/dev/null 2>&1; then
        cat >&2 <<EOF
error: TAP interface $TAP_IFACE does not exist

setup example:
  sudo ip tuntap add dev $TAP_IFACE mode tap user $USER
  sudo ip addr add $TAP_HOST_IP/24 dev $TAP_IFACE
  sudo ip link set $TAP_IFACE up
EOF
        exit 1
    fi
    if ! ip -4 addr show dev "$TAP_IFACE" | grep -q "$TAP_HOST_IP/24"; then
        cat >&2 <<EOF
error: TAP interface $TAP_IFACE does not have $TAP_HOST_IP/24

setup example:
  sudo ip addr add $TAP_HOST_IP/24 dev $TAP_IFACE
  sudo ip link set $TAP_IFACE up
EOF
        exit 1
    fi
}

# 汇总一个场景的全部 run log（跨 --repeat），输出干净的 mean/stddev 报告。
summarize_scenario() {
    local scenario="$1"; shift
    local summary_file="$RESULTS_DIR/summary-$ARCH-$scenario-$TIMESTAMP.txt"
    if ! command -v python3 >/dev/null 2>&1; then
        echo "warning: python3 not found, skipping auto-summary" >&2
        return
    fi
    echo "=== net-bench: summary ($scenario) -> $summary_file ==="
    python3 "$SUMMARIZER" "$@" | tee "$summary_file"
}

run_one() {
    local scenario="$1" bind_addr="" test_case="net-bench" qemu_config=() env_vars=() server_log
    case "$scenario" in
        slirp)
            ;;
        slirp-smp4)
            qemu_config=(--qemu-config "apps/starry/net-bench/qemu-aarch64-smp4.toml")
            ;;
        tap)
            check_tap
            bind_addr="$TAP_HOST_IP"
            qemu_config=(--qemu-config "apps/starry/net-bench/qemu-aarch64-tap.toml")
            env_vars=(AX_IP=192.168.100.2 AX_GW="$TAP_HOST_IP" AX_PREFIX_LEN=24)
            ;;
        *)
            usage
            exit 1
            ;;
    esac

    write_fingerprint "$RESULTS_DIR/fingerprint-$ARCH-$scenario-$TIMESTAMP.txt"

    local run_logs=()
    local rep
    for ((rep = 1; rep <= REPEAT; rep++)); do
        local result_file="$RESULTS_DIR/starry-$ARCH-$scenario-$TIMESTAMP-r${rep}.txt"
        server_log="$RESULTS_DIR/iperf3-server-$scenario-$TIMESTAMP-r${rep}.log"
        start_iperf3_server "$bind_addr" "$server_log"
        # shellcheck disable=SC2064
        trap "kill $iperf3_pid 2>/dev/null || true" RETURN

        echo "=== net-bench: running StarryOS $test_case ($ARCH, $scenario, repeat $rep/$REPEAT) ==="
        # Sample eBPF net_stats before bench (appended to result_file as NET_STATS_BEGIN/END block).
        if command -v net_stats >/dev/null 2>&1; then
            echo "=== net-bench: sampling net_stats (before) ===" | tee -a "$result_file"
            timeout 6 net_stats --once 2>/dev/null | tee -a "$result_file" || true
        fi
        (cd "$WORKSPACE" && env "${env_vars[@]}" cargo xtask starry app qemu --test-case "$test_case" --arch "$ARCH" "${qemu_config[@]}") \
            2>&1 | tee "$result_file"
        # Sample eBPF net_stats after bench.
        if command -v net_stats >/dev/null 2>&1; then
            echo "=== net-bench: sampling net_stats (after) ===" | tee -a "$result_file"
            timeout 6 net_stats --once 2>/dev/null | tee -a "$result_file" || true
        fi

        kill "$iperf3_pid" 2>/dev/null || true
        trap - RETURN
        run_logs+=("$result_file")
        echo "=== Results saved to $result_file ==="
    done

    summarize_scenario "$scenario" "${run_logs[@]}"
}

if [[ "$ARCH" == "help" ]]; then
    usage
    exit 0
fi

command -v iperf3 >/dev/null 2>&1 || { echo "error: iperf3 not found on host (apt install iperf3)" >&2; exit 1; }
check_arch

case "$SCENARIO" in
    all)
        run_one slirp
        run_one slirp-smp4
        run_one tap
        ;;
    slirp|slirp-smp4|tap)
        run_one "$SCENARIO"
        ;;
    help)
        usage
        ;;
    *)
        usage
        exit 1
        ;;
esac
