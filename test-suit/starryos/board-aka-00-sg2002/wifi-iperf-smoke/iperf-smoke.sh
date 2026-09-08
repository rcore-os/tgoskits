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

report=$(mktemp) || fail
trap 'rm -f "$report"' EXIT
iperf3 -c "$server_ip" -t 20 -O 2 -P 1 -l 128K >"$report" 2>&1
status=$?
cat "$report"
[ "$status" -eq 0 ] || fail

# A completed 128 KiB write can straddle a reporting interval. Allow that
# jitter, but reject three seconds without progress after the warm-up.
awk '
    /omitted/ { next }
    !sub(/^\[[[:space:]]*[0-9]+\][[:space:]]*/, "") { next }
    $NF == "receiver" { received = ($3 + 0 > 0); next }
    $NF == "sender" { next }
    $1 ~ /^[0-9]+\.[0-9]+-[0-9]+\.[0-9]+$/ && $2 == "sec" {
        split($1, interval, "-")
        if ($3 + 0 > 0) {
            stalled = 0
            progressed = 1
        } else {
            stalled += interval[2] - interval[1]
        }
        if (stalled >= 3) {
            print "Wi-Fi transfer stalled for at least three seconds"
            exit 1
        }
        end = interval[2]
    }
    END { if (!progressed || !received || end < 19) exit 1 }
' "$report" || fail

echo STARRY_AKA_WIFI_IPERF_SMOKE_PASSED
