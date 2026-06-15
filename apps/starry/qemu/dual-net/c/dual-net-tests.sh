#!/bin/sh
set -eu

fail() {
    echo "DUAL_NET_TEST_FAILED: $*"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command $1"
}

now_ms() {
    ns="$(date +%s%N 2>/dev/null || true)"
    case "$ns" in
        *[!0-9]* | "")
            echo "$(($(date +%s) * 1000))"
            ;;
        *)
            echo "$((ns / 1000000))"
            ;;
    esac
}

iface_addr_contains() {
    iface="$1"
    expected="$2"
    ifconfig "$iface" 2>&1 | tee "/tmp/dual-net-$iface.ifconfig"
    ip addr show "$iface" 2>&1 | tee "/tmp/dual-net-$iface.ipaddr"
    grep -qF "$expected" "/tmp/dual-net-$iface.ifconfig" ||
        grep -qF "$expected" "/tmp/dual-net-$iface.ipaddr"
}

fetch_with_iface() {
    iface="$1"
    host="$2"
    tag="$3"
    out="/tmp/dual-net-$tag.bin"
    meta="/tmp/dual-net-$tag.meta"
    start="$(now_ms)"
    curl --interface "$iface" \
        --connect-timeout 10 \
        --max-time 60 \
        --fail \
        --silent \
        --show-error \
        "http://$host:18382/payload.bin?iface=$iface&tag=$tag" \
        -o "$out"
    end="$(now_ms)"
    bytes="$(wc -c < "$out" | tr -d ' ')"
    elapsed="$((end - start))"
    printf '%s %s %s\n' "$elapsed" "$bytes" "$out" > "$meta"
}

wait_fetch() {
    pid="$1"
    tag="$2"
    if ! wait "$pid"; then
        fail "fetch $tag failed"
    fi
    read -r elapsed bytes out < "/tmp/dual-net-$tag.meta"
    [ "$bytes" -ge 1048576 ] || fail "fetch $tag too small: $bytes"
    echo "DUAL_NET_FETCH_${tag}_MS=$elapsed BYTES=$bytes"
    rm -f "$out" "/tmp/dual-net-$tag.meta"
}

echo "DUAL_NET_TEST_BEGIN"
need_cmd ifconfig
need_cmd ip
need_cmd curl

if iface_addr_contains eth0 10.0.2.15; then
    echo "DUAL_NET_ETH0_ADDR_OK"
else
    fail "eth0 did not get 10.0.2.15"
fi

if iface_addr_contains eth1 10.0.3.15; then
    echo "DUAL_NET_ETH1_ADDR_OK"
else
    fail "eth1 did not get 10.0.3.15"
fi

single_start="$(now_ms)"
fetch_with_iface eth0 10.0.2.2 eth0_single
fetch_with_iface eth1 10.0.3.2 eth1_single
single_end="$(now_ms)"
read -r eth0_single_ms eth0_single_bytes _ < /tmp/dual-net-eth0_single.meta
read -r eth1_single_ms eth1_single_bytes _ < /tmp/dual-net-eth1_single.meta
echo "DUAL_NET_FETCH_ETH0_SINGLE_MS=$eth0_single_ms BYTES=$eth0_single_bytes"
echo "DUAL_NET_FETCH_ETH1_SINGLE_MS=$eth1_single_ms BYTES=$eth1_single_bytes"
echo "DUAL_NET_FETCH_SERIAL_MS=$((single_end - single_start))"
rm -f /tmp/dual-net-eth0_single.bin /tmp/dual-net-eth1_single.bin
rm -f /tmp/dual-net-eth0_single.meta /tmp/dual-net-eth1_single.meta

parallel_start="$(now_ms)"
fetch_with_iface eth0 10.0.2.2 ETH0_PARALLEL &
pid0="$!"
fetch_with_iface eth1 10.0.3.2 ETH1_PARALLEL &
pid1="$!"
wait_fetch "$pid0" ETH0_PARALLEL
wait_fetch "$pid1" ETH1_PARALLEL
parallel_end="$(now_ms)"
echo "DUAL_NET_FETCH_PARALLEL_MS=$((parallel_end - parallel_start))"

echo "DUAL_NET_TEST_PASSED"
