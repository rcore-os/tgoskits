#!/bin/sh
set -eu

mosquitto_pid=""
test_done=0

# Pre-cleanup: kill any leftover mosquitto server processes and stale temp files!
# Use pidof for exact name match; pkill would also match this script's name "mosquitto-*"
for pid in $(pidof mosquitto 2>/dev/null); do
    kill -9 "$pid" 2>/dev/null || true
done
rm -rf /var/lib/mosquitto/mosquitto* 2>/dev/null || true
sleep 1

start_mosquitto() {
    # Kill any existing mosquitto processes (handles auto-started instances)
    for pid in $(pidof mosquitto 2>/dev/null); do
        kill -9 "$pid" 2>/dev/null || true
    done
    sleep 2

    mkdir -p /var/lib/mosquitto/mosquitto
    chmod 777 /var/lib/mosquitto/mosquitto
    cat > /var/lib/mosquitto/mosquitto-test.conf << 'EOF'
listener 1883
socket_domain ipv4
allow_anonymous true
persistence false
log_dest stderr
log_type error
log_type warning
connection_messages true
EOF

    /usr/sbin/mosquitto -c /var/lib/mosquitto/mosquitto-test.conf > /var/lib/mosquitto/mosquitto-smoke.log 2>&1 &
    mosquitto_pid=$!

    i=0
    while [ "$i" -lt 30 ]; do
        if mosquitto_pub -h 127.0.0.1 -p 1883 -t test/ping -m "ping" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    echo "=== mosquitto-smoke.log ==="
    cat /var/lib/mosquitto/mosquitto-smoke.log
    echo "MOSQUITTO_SMOKE_TEST_FAILED"
    exit 1
}

stop_mosquitto() {
    if [ -n "$mosquitto_pid" ]; then
        kill "$mosquitto_pid" >/dev/null 2>&1 || true
        i=0
        while kill -0 "$mosquitto_pid" >/dev/null 2>&1 && [ "$i" -lt 10 ]; do
            i=$((i + 1))
            sleep 1
        done
        kill -9 "$mosquitto_pid" >/dev/null 2>&1 || true
        wait "$mosquitto_pid" >/dev/null 2>&1 || true
        mosquitto_pid=""
    fi
}

cleanup() {
    stop_mosquitto
    # Kill any remaining mosquitto processes (including stale ones)
    for pid in $(pidof mosquitto 2>/dev/null); do
        kill -9 "$pid" 2>/dev/null || true
    done
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "MOSQUITTO_SMOKE_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

echo "MOSQUITTO_SMOKE_STAGE start"
start_mosquitto

echo "MOSQUITTO_SMOKE_STAGE basic-pub-sub"
# Test basic publish/subscribe
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/smoke -C 1 -W 5 > /var/lib/mosquitto/smoke-sub.out 2>&1 &
sub_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/smoke -m "hello-smoke"
wait "$sub_pid" || true
grep -q 'hello-smoke' /var/lib/mosquitto/smoke-sub.out

echo "MOSQUITTO_SMOKE_STAGE multiple-topics"
# Test multiple topics
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/topic1 -C 1 -W 5 > /var/lib/mosquitto/smoke-topic1.out 2>&1 &
sub1_pid=$!
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/topic2 -C 1 -W 5 > /var/lib/mosquitto/smoke-topic2.out 2>&1 &
sub2_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/topic1 -m "msg1"
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/topic2 -m "msg2"
wait "$sub1_pid" || true
wait "$sub2_pid" || true
grep -q 'msg1' /var/lib/mosquitto/smoke-topic1.out
grep -q 'msg2' /var/lib/mosquitto/smoke-topic2.out

echo "MOSQUITTO_SMOKE_STAGE wildcard-sub"
# Test wildcard subscription
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/wildcard/# -C 2 -W 5 > /var/lib/mosquitto/smoke-wildcard.out 2>&1 &
sub_wc_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/wildcard/a -m "wild-a"
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/wildcard/b -m "wild-b"
wait "$sub_wc_pid" || true
grep -q 'wild-a' /var/lib/mosquitto/smoke-wildcard.out
grep -q 'wild-b' /var/lib/mosquitto/smoke-wildcard.out

echo "MOSQUITTO_SMOKE_STAGE qos-levels"
# Test QoS 0
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/qos0 -q 0 -C 1 -W 5 > /var/lib/mosquitto/smoke-qos0.out 2>&1 &
sub_qos0_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/qos0 -q 0 -m "qos0-msg"
wait "$sub_qos0_pid" || true
grep -q 'qos0-msg' /var/lib/mosquitto/smoke-qos0.out

# Test QoS 1
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/qos1 -q 1 -C 1 -W 5 > /var/lib/mosquitto/smoke-qos1.out 2>&1 &
sub_qos1_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/qos1 -q 1 -m "qos1-msg"
wait "$sub_qos1_pid" || true
grep -q 'qos1-msg' /var/lib/mosquitto/smoke-qos1.out

echo "MOSQUITTO_SMOKE_STAGE last-will-testament"
# Test Last Will and Testament - embedded devices use LWT to notify offline status
# Subscribe to LWT topic first
mosquitto_sub -h 127.0.0.1 -p 1883 -t devices/status -C 1 -W 10 > /var/lib/mosquitto/smoke-lwt.out 2>&1 &
sub_lwt_pid=$!
sleep 1
# Start a client with LWT message set; must use SIGKILL (-9) to trigger LWT (clean disconnect won't)
mosquitto_sub -h 127.0.0.1 -p 1883 -t dummy -i "lwt-device" --will-topic devices/status --will-payload "device-offline" --will-qos 1 --will-retain -W 30 > /dev/null 2>&1 &
lwt_client_pid=$!
sleep 2
# SIGKILL forces unexpected disconnect, which triggers the will message
kill -9 "$lwt_client_pid" 2>/dev/null || true
wait "$lwt_client_pid" 2>/dev/null || true
wait "$sub_lwt_pid" || true
grep -q 'device-offline' /var/lib/mosquitto/smoke-lwt.out

echo "MOSQUITTO_SMOKE_STAGE keepalive"
# Test keep-alive - embedded devices need periodic ping to maintain connection
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/keepalive -C 1 -W 10 -k 5 > /var/lib/mosquitto/smoke-keepalive.out 2>&1 &
sub_ka_pid=$!
sleep 1
# Client with 5s keep-alive sends pings automatically (minimum allowed)
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/keepalive -m "alive-msg" -i "keepalive-pub" -k 5
wait "$sub_ka_pid" || true
grep -q 'alive-msg' /var/lib/mosquitto/smoke-keepalive.out

echo "MOSQUITTO_SMOKE_STAGE sensor-data"
# Test typical embedded sensor data pattern (small JSON payloads)
mosquitto_sub -h 127.0.0.1 -p 1883 -t devices/sensor01/telemetry -C 3 -W 5 > /var/lib/mosquitto/smoke-sensor.out 2>&1 &
sub_sensor_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor01/telemetry -m '{"temp":22.5,"hum":65.2,"ts":1717000000}'
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor01/telemetry -m '{"temp":22.6,"hum":65.0,"ts":1717000001}'
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor01/telemetry -m '{"temp":22.4,"hum":65.5,"ts":1717000002}'
wait "$sub_sensor_pid" || true
grep -q '"temp":22.5' /var/lib/mosquitto/smoke-sensor.out
grep -q '"temp":22.6' /var/lib/mosquitto/smoke-sensor.out
grep -q '"temp":22.4' /var/lib/mosquitto/smoke-sensor.out

test_done=1
trap - EXIT
cleanup

echo "MOSQUITTO_SMOKE_TEST_PASSED"
