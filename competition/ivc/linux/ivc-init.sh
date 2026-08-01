#!/bin/sh

set -eu

mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
exec >/dev/console 2>&1

kernel_argument() {
    key="$1"
    default_value="$2"
    for argument in $(cat /proc/cmdline); do
        case "$argument" in
            "$key"=*)
                echo "${argument#*=}"
                return
                ;;
        esac
    done
    echo "$default_value"
}

mode=$(kernel_argument ivc.mode neural)
count=$(kernel_argument ivc.count 1800)
period_ms=$(kernel_argument ivc.period_ms 100)

echo "IVC-LINUX-BOOT mode=$mode count=$count period_ms=$period_ms"

attempt=0
while [ "$attempt" -lt 60 ] && [ ! -e /sys/class/net/eth0/address ]; do
    attempt=$((attempt + 1))
    sleep 1
done
if [ ! -e /sys/class/net/eth0/address ]; then
    echo "IVC-LINUX-FATAL reason=eth0-not-found"
    exec sh
fi

mac=$(cat /sys/class/net/eth0/address)
ip link set eth0 down
ip addr flush dev eth0 2>/dev/null || true
ip addr add 10.0.0.1/24 dev eth0
ip link set eth0 up
ip route replace 10.0.0.0/24 dev eth0

echo "IVC-LINUX-NET iface=eth0 mac=$mac ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1"
if /usr/local/bin/ivcproto controller 10.0.0.2:5500 "$count" "$mode" "$period_ms"; then
    result=0
else
    result=$?
fi
echo "IVC-LINUX-DONE exit=$result"

# Keep the guest available for console inspection without asking the shared
# QEMU machine to power off.
while true; do
    sleep 60
done
