#!/bin/sh
set -eu

fail() {
    echo "DUAL_NET_TEST_FAILED: $*"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing command $1"
}

APK_STRESS_PACKAGE="${APK_STRESS_PACKAGE:-python3}"
APK_STRESS_MIN_BYTES="${APK_STRESS_MIN_BYTES:-8388608}"
APK_STRESS_RETRIES="${APK_STRESS_RETRIES:-3}"

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

prepare_apk_repositories() {
    if [ -f /etc/apk/repositories ]; then
        sed -i 's|https://|http://|g' /etc/apk/repositories
        echo "DUAL_NET_APK_REPOSITORIES_BEGIN"
        cat /etc/apk/repositories
        echo "DUAL_NET_APK_REPOSITORIES_END"
    fi
}

retry_cmd() {
    desc="$1"
    shift
    attempt=1
    while [ "$attempt" -le "$APK_STRESS_RETRIES" ]; do
        if "$@"; then
            return 0
        fi
        echo "DUAL_NET_RETRY: $desc attempt=$attempt/$APK_STRESS_RETRIES failed"
        attempt="$((attempt + 1))"
        sleep 2
    done
    return 1
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

apk_fetch_verify() {
    dir="/tmp/dual-net-apk-fetch"
    sum="/tmp/dual-net-apk-fetch.sha256"
    rm -rf "$dir" "$sum"
    mkdir -p "$dir"

    prepare_apk_repositories

    echo "DUAL_NET_APK_UPDATE_BEGIN"
    retry_cmd "apk update" apk update ||
        fail "apk update failed"
    echo "DUAL_NET_APK_UPDATE_DONE"

    start="$(now_ms)"
    retry_cmd "apk fetch $APK_STRESS_PACKAGE" apk fetch -R -o "$dir" "$APK_STRESS_PACKAGE" ||
        fail "apk fetch failed for $APK_STRESS_PACKAGE"
    end="$(now_ms)"

    total=0
    count=0
    : > "$sum"
    for pkg in "$dir"/*.apk; do
        [ -e "$pkg" ] || fail "apk fetch produced no .apk files"
        apk verify "$pkg" || fail "apk verify failed for $pkg"
        sha256sum "$pkg" >> "$sum"
        bytes="$(wc -c < "$pkg" | tr -d ' ')"
        total="$((total + bytes))"
        count="$((count + 1))"
    done

    [ "$total" -ge "$APK_STRESS_MIN_BYTES" ] ||
        fail "apk fetch too small: total=$total min=$APK_STRESS_MIN_BYTES"

    sha256sum -c "$sum" ||
        fail "apk sha256 verification failed"

    echo "DUAL_NET_APK_FETCH_MS=$((end - start)) BYTES=$total PACKAGES=$count PACKAGE=$APK_STRESS_PACKAGE"
    rm -rf "$dir" "$sum"
}

echo "DUAL_NET_TEST_BEGIN"
need_cmd ifconfig
need_cmd ip
need_cmd curl
need_cmd apk
need_cmd sha256sum

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

apk_fetch_verify

echo "DUAL_NET_TEST_PASSED"
