#!/usr/bin/env bash
# apps/starry/net-bench/run.sh — StarryOS 网络性能测试入口
# 用法: bash apps/starry/net-bench/run.sh [arch] [scenario]
#   arch 当前仅支持 aarch64
#   scenario 默认 slirp，可选：slirp, slirp-smp4, tap, all
set -euo pipefail

ARCH="${1:-aarch64}"
SCENARIO="${2:-slirp}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
IPERF3_PORT=5201
TAP_IFACE="${TAP_IFACE:-tap0}"
TAP_HOST_IP="${TAP_HOST_IP:-192.168.100.1}"

mkdir -p "$RESULTS_DIR"

usage() {
    cat >&2 <<EOF
usage: bash apps/starry/net-bench/run.sh [arch] [scenario]

scenario:
  slirp       QEMU usermode networking, smp=1 (default, smoke only)
  slirp-smp4  QEMU usermode networking, smp=4
  tap         TAP direct networking, requires $TAP_IFACE=$TAP_HOST_IP/24
  all         Run slirp, slirp-smp4, then tap
EOF
}

check_arch() {
    if [[ "$ARCH" != "aarch64" ]]; then
        echo "error: apps/starry/net-bench currently only provides aarch64 QEMU configs" >&2
        exit 1
    fi
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

run_one() {
    local scenario="$1" bind_addr="" test_case="net-bench" qemu_config=() env_vars=() result_file server_log
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

    result_file="$RESULTS_DIR/starry-$ARCH-$scenario-$TIMESTAMP.txt"
    server_log="$RESULTS_DIR/iperf3-server-$scenario-$TIMESTAMP.log"
    start_iperf3_server "$bind_addr" "$server_log"
    trap "kill $iperf3_pid 2>/dev/null || true" RETURN

    echo "=== net-bench: running StarryOS $test_case ($ARCH, $scenario) ==="
    (cd "$WORKSPACE" && env "${env_vars[@]}" cargo xtask starry app qemu --test-case "$test_case" --arch "$ARCH" "${qemu_config[@]}") \
        2>&1 | tee "$result_file"

    echo "=== Results saved to $result_file ==="
}

if [[ "$ARCH" == "-h" || "$ARCH" == "--help" || "$ARCH" == "help" ]]; then
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
    -h|--help|help)
        usage
        ;;
    *)
        usage
        exit 1
        ;;
esac
