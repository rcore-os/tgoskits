#!/bin/sh
set -eu

IFACE="${NETWORK_TEST_IFACE:-eth0}"
ADDR="${NETWORK_TEST_ADDR:-10.0.2.15}"
CIDR="${NETWORK_TEST_CIDR:-10.0.2.15/24}"
NETMASK="${NETWORK_TEST_NETMASK:-255.255.255.0}"
GATEWAY="${NETWORK_TEST_GATEWAY:-10.0.2.2}"
EXTERNAL_HOST="${NETWORK_TEST_EXTERNAL_HOST:-www.baidu.com}"

fail() {
    echo "NETWORK_TEST_FAILED: $*"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"
}

run() {
    echo "NETWORK_CMD: $*"
    "$@" 2>&1
}

run_may_fail() {
    echo "NETWORK_CMD_MAY_FAIL: $*"
    "$@" 2>&1 || echo "NETWORK_CMD_EXPECTED_TOLERATED_RC: $* rc=$?"
}

expect_output_contains() {
    file="$1"
    pattern="$2"
    desc="$3"
    grep -q "$pattern" "$file" || {
        cat "$file"
        fail "$desc"
    }
    echo "NETWORK_CHECK_OK: $desc"
}

show_state() {
    tag="$1"
    echo "NETWORK_STATE_BEGIN: $tag"
    run ifconfig | tee "/tmp/network-${tag}-ifconfig-all.out"
    run ifconfig "$IFACE" | tee "/tmp/network-${tag}-ifconfig-${IFACE}.out"
    run ip addr show | tee "/tmp/network-${tag}-ip-addr.out"
    run ip link show "$IFACE" | tee "/tmp/network-${tag}-ip-link-${IFACE}.out"
    echo "NETWORK_STATE_END: $tag"
}

check_eth0_up_with_addr() {
    tag="$1"
    ifconfig "$IFACE" >"/tmp/network-${tag}-ifconfig-check.out" 2>&1
    ip addr show "$IFACE" >"/tmp/network-${tag}-ip-check.out" 2>&1
    expect_output_contains "/tmp/network-${tag}-ifconfig-check.out" "$ADDR" \
        "$IFACE has $ADDR in ifconfig"
    expect_output_contains "/tmp/network-${tag}-ip-check.out" "$ADDR" \
        "$IFACE has $ADDR in ip addr"
}

expect_ping_ok() {
    target="$1"
    desc="$2"
    out="/tmp/network-ping-ok-$(echo "$desc" | tr ' /' '__').out"
    run ping -c 1 -W 3 "$target" | tee "$out"
    expect_output_contains "$out" "0% packet loss" "$desc"
}

expect_ping_fail() {
    target="$1"
    desc="$2"
    out="/tmp/network-ping-fail-$(echo "$desc" | tr ' /' '__').out"
    echo "NETWORK_CMD_EXPECT_FAIL: ping -c 1 -W 2 $target"
    if ping -c 1 -W 2 "$target" >"$out" 2>&1; then
        cat "$out"
        fail "$desc unexpectedly succeeded"
    fi
    cat "$out"
    echo "NETWORK_CHECK_OK: $desc"
}

echo "NETWORK_TEST_BEGIN"
need_cmd ifconfig
need_cmd ip
need_cmd ping

show_state initial
check_eth0_up_with_addr initial
expect_ping_ok "$GATEWAY" "initial $IFACE up can ping gateway $GATEWAY"

echo "NETWORK_STAGE_EXTERNAL_PING_BEGIN"
run ping -c 3 -W 8 "$EXTERNAL_HOST" | tee /tmp/network-ping-external.out
expect_output_contains /tmp/network-ping-external.out "0% packet loss" \
    "ping external host $EXTERNAL_HOST has 0% packet loss"
echo "NETWORK_STAGE_EXTERNAL_PING_DONE"

echo "NETWORK_STAGE_IOCTL_BEGIN"
run ifconfig "$IFACE" "$ADDR" netmask "$NETMASK" up
check_eth0_up_with_addr ioctl_set_addr
run ifconfig "$IFACE" down
expect_ping_fail "$GATEWAY" "$IFACE down cannot ping gateway $GATEWAY"
run ifconfig "$IFACE" up
check_eth0_up_with_addr ioctl_up_after_down
expect_ping_ok "$GATEWAY" "$IFACE up can ping gateway $GATEWAY"
run ifconfig "$IFACE" 0.0.0.0
expect_ping_fail "$GATEWAY" "$IFACE without IPv4 address cannot ping gateway $GATEWAY"
run ifconfig "$IFACE" "$ADDR" netmask "$NETMASK" up
check_eth0_up_with_addr ioctl_restore_addr
expect_ping_ok "$GATEWAY" "$IFACE address restored can ping gateway $GATEWAY"
echo "NETWORK_STAGE_IOCTL_DONE"

echo "NETWORK_STAGE_NETLINK_BEGIN"
run_may_fail ip addr del "$CIDR" dev "$IFACE"
run ip addr add "$CIDR" dev "$IFACE"
check_eth0_up_with_addr netlink_add_addr
expect_ping_ok "$GATEWAY" "netlink add address can ping gateway $GATEWAY"
run ip addr del "$CIDR" dev "$IFACE"
expect_ping_fail "$GATEWAY" "netlink delete address cannot ping gateway $GATEWAY"
run ip addr add "$CIDR" dev "$IFACE"
run ip link set "$IFACE" down
expect_ping_fail "$GATEWAY" "netlink link down cannot ping gateway $GATEWAY"
run ip link set "$IFACE" up
run_may_fail ip route replace default via "$GATEWAY" dev "$IFACE"
check_eth0_up_with_addr netlink_restore_addr
expect_ping_ok "$GATEWAY" "netlink link up and address restored can ping gateway $GATEWAY"
echo "NETWORK_STAGE_NETLINK_DONE"

show_state final

echo "NETWORK_STAGE_PING_BEGIN"
run_may_fail ip route replace default via "$GATEWAY" dev "$IFACE"
run ping -c 3 -W 5 "$GATEWAY" | tee /tmp/network-ping-gateway.out
expect_output_contains /tmp/network-ping-gateway.out "0% packet loss" \
    "ping gateway $GATEWAY has 0% packet loss"
echo "NETWORK_STAGE_PING_DONE"

echo "NETWORK_TEST_PASSED"
