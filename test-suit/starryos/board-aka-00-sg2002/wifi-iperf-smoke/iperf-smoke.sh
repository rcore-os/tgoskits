#!/bin/sh
set -u

fail() {
    echo STARRY_AKA_WIFI_IPERF_SMOKE_FAILED
    exit 1
}

[ "$#" -eq 1 ] && [ -n "$1" ] || fail
server_ip=$1

attempt=1
while [ "$attempt" -le 60 ]; do
    if ip -4 -o addr show dev wlan0 | grep -q ' inet '; then
        break
    fi
    sleep 1
    attempt=$((attempt + 1))
done
if ! ip -4 -o addr show dev wlan0 | grep -q ' inet '; then
    echo STARRY_AKA_WIFI_DHCP_FAILED
    fail
fi

command -v iperf3 >/dev/null 2>&1 || fail

iperf3 -c "$server_ip" -t 3 -O 1 -P 1 -l 128K || fail

echo STARRY_AKA_WIFI_IPERF_SMOKE_PASSED
