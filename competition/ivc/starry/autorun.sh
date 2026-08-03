#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/ivc-profile

exec >/dev/console 2>&1

fatal() {
    echo "IVC-STARRY-FATAL reason=$1"
    "$BB" sync
    "$BB" poweroff -f
    while true; do
        "$BB" sleep 60
    done
}

[ -x "$BB" ] || fatal busybox-not-found
[ -r "$PROFILE" ] || fatal profile-not-found
. "$PROFILE"

case "${ivc_mode:-}" in
    neural|manual) ;;
    *) fatal invalid-controller-mode ;;
esac
case "${ivc_backend:-}" in
    native) ;;
    onnxruntime) fatal onnxruntime-backend-not-installed ;;
    *) fatal invalid-inference-backend ;;
esac
case "${ivc_count:-}" in
    ''|*[!0-9]*) fatal invalid-command-count ;;
esac
case "${ivc_period_ms:-}" in
    ''|*[!0-9]*) fatal invalid-period ;;
esac
[ "${ivc_raw_csv:-}" = /var/lib/ivc/raw.csv ] || fatal invalid-raw-csv-path

cpu_count=$($BB grep -c '^processor' /proc/cpuinfo 2>/dev/null || true)
[ "$cpu_count" -ge 2 ] || fatal insufficient-vcpus

echo "IVC-STARRY-BOOT mode=$ivc_mode backend=$ivc_backend count=$ivc_count period_ms=$ivc_period_ms vcpus=$cpu_count"

attempt=0
while [ "$attempt" -lt 60 ]; do
    if "$BB" ip link show dev eth0 >/dev/null 2>&1; then
        break
    fi
    attempt=$((attempt + 1))
    "$BB" sleep 1
done
[ "$attempt" -lt 60 ] || fatal eth0-not-found

# Starry brings the kernel-owned interface up during ax_net initialization.
# BusyBox implements `ip link set` through SIOCSIFFLAGS, which Starry does not
# expose for this interface; address configuration uses the supported rtnetlink
# path and does not require toggling the already-active link.
"$BB" ip addr flush dev eth0 >/dev/null 2>&1 || true
"$BB" ip addr add 10.0.0.1/24 dev eth0 || fatal eth0-address-failed

mac=$($BB cat /sys/class/net/eth0/address 2>/dev/null || echo unknown)
echo "IVC-STARRY-NET iface=eth0 mac=$mac ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1"

if /usr/local/bin/ivcproto controller \
    10.0.0.2:5500 "$ivc_count" "$ivc_mode" "$ivc_period_ms" \
    --backend "$ivc_backend" --raw-csv "$ivc_raw_csv"; then
    [ -r "$ivc_raw_csv" ] || fatal raw-csv-not-found
    raw_lines=$("$BB" wc -l < "$ivc_raw_csv") || fatal raw-csv-count-failed
    expected_raw_lines=$((ivc_count + 1))
    [ "$raw_lines" -eq "$expected_raw_lines" ] || fatal raw-csv-count-mismatch
    raw_checksum=$("$BB" sha256sum "$ivc_raw_csv") || fatal raw-csv-hash-failed
    raw_sha256=${raw_checksum%% *}
    echo "IVC-STARRY-RAW path=$ivc_raw_csv samples=$ivc_count sha256=$raw_sha256"
    result=0
else
    result=$?
fi
"$BB" sync || fatal final-sync-failed
echo "IVC-STARRY-DONE exit=$result"
"$BB" poweroff -f
"$BB" sleep 5
fatal poweroff-returned
