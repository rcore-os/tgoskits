#!/bin/sh
set -eu

mosquitto_pid=""
auth_pid=""
test_done=0

# Pre-cleanup: kill any leftover mosquitto server processes and stale temp files
# Use pidof for exact name match; pkill would also match this script's name "mosquitto-*"
for pid in $(pidof mosquitto 2>/dev/null); do
    kill -9 "$pid" 2>/dev/null || true
done
rm -rf /var/lib/mosquitto/mosquitto* 2>/dev/null || true
sleep 1

start_mosquitto() {
    config_file="$1"

    # Kill any existing mosquitto processes (handles auto-started instances)
    for pid in $(pidof mosquitto 2>/dev/null); do
        kill -9 "$pid" 2>/dev/null || true
    done
    sleep 2

    mkdir -p /var/lib/mosquitto/mosquitto
    chmod 777 /var/lib/mosquitto/mosquitto
    cat > "$config_file" << 'EOF'
listener 1883
socket_domain ipv4
allow_anonymous true
persistence true
persistence_location /var/lib/mosquitto/mosquitto/
log_dest stderr
log_type error
log_type warning
log_type notice
log_type information
connection_messages true
max_inflight_messages 20
max_queued_messages 1000
EOF

    /usr/sbin/mosquitto -c "$config_file" > /var/lib/mosquitto/mosquitto.log 2>&1 &
    mosquitto_pid=$!

    i=0
    while [ "$i" -lt 30 ]; do
        if mosquitto_pub -h 127.0.0.1 -p 1883 -t test/ping -m "ping" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    echo "=== mosquitto.log ==="
    cat /var/lib/mosquitto/mosquitto.log
    echo "MOSQUITTO_TEST_FAILED"
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

stop_auth_mosquitto() {
    if [ -n "$auth_pid" ]; then
        kill "$auth_pid" >/dev/null 2>&1 || true
        wait "$auth_pid" >/dev/null 2>&1 || true
        auth_pid=""
    fi
}

cleanup() {
    stop_auth_mosquitto
    stop_mosquitto
    # Kill any remaining mosquitto processes (including stale ones)
    for pid in $(pidof mosquitto 2>/dev/null); do
        kill -9 "$pid" 2>/dev/null || true
    done
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "MOSQUITTO_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail_with_file() {
    path="$1"
    echo "=== $path ==="
    cat "$path" 2>/dev/null || true
    echo "MOSQUITTO_TEST_FAILED"
    exit 1
}

echo "MOSQUITTO_STAGE start"
start_mosquitto /var/lib/mosquitto/mosquitto-test.conf

echo "MOSQUITTO_STAGE basic-pub-sub"
# Test basic publish/subscribe
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/basic -C 1 -W 5 > /var/lib/mosquitto/basic-sub.out 2>&1 &
sub_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/basic -m "hello-mosquitto"
wait "$sub_pid" || true
grep -q 'hello-mosquitto' /var/lib/mosquitto/basic-sub.out

echo "MOSQUITTO_STAGE multiple-messages"
# Test multiple messages
msg_count=10
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/multi -C "$msg_count" -W 10 > /var/lib/mosquitto/multi-sub.out 2>&1 &
sub_multi_pid=$!
sleep 1
i=0
while [ "$i" -lt "$msg_count" ]; do
    mosquitto_pub -h 127.0.0.1 -p 1883 -t test/multi -m "message-$i"
    i=$((i + 1))
done
wait "$sub_multi_pid" || true
i=0
while [ "$i" -lt "$msg_count" ]; do
    grep -q "message-$i" /var/lib/mosquitto/multi-sub.out
    i=$((i + 1))
done

echo "MOSQUITTO_STAGE wildcard-topics"
# Test wildcard subscriptions
mosquitto_sub -h 127.0.0.1 -p 1883 -t sensor/+/temperature -C 3 -W 5 > /var/lib/mosquitto/wildcard-sub.out 2>&1 &
sub_wc_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t sensor/room1/temperature -m "22.5"
mosquitto_pub -h 127.0.0.1 -p 1883 -t sensor/room2/temperature -m "23.1"
mosquitto_pub -h 127.0.0.1 -p 1883 -t sensor/room3/temperature -m "21.8"
wait "$sub_wc_pid" || true
grep -q '22.5' /var/lib/mosquitto/wildcard-sub.out
grep -q '23.1' /var/lib/mosquitto/wildcard-sub.out
grep -q '21.8' /var/lib/mosquitto/wildcard-sub.out

echo "MOSQUITTO_STAGE qos-levels"
# Test QoS 0
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/qos0 -q 0 -C 1 -W 5 > /var/lib/mosquitto/qos0-sub.out 2>&1 &
sub_qos0_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/qos0 -q 0 -m "qos0-message"
wait "$sub_qos0_pid" || true
grep -q 'qos0-message' /var/lib/mosquitto/qos0-sub.out

# Test QoS 1
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/qos1 -q 1 -C 1 -W 5 > /var/lib/mosquitto/qos1-sub.out 2>&1 &
sub_qos1_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/qos1 -q 1 -m "qos1-message"
wait "$sub_qos1_pid" || true
grep -q 'qos1-message' /var/lib/mosquitto/qos1-sub.out

# Test QoS 2
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/qos2 -q 2 -C 1 -W 5 > /var/lib/mosquitto/qos2-sub.out 2>&1 &
sub_qos2_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/qos2 -q 2 -m "qos2-message"
wait "$sub_qos2_pid" || true
grep -q 'qos2-message' /var/lib/mosquitto/qos2-sub.out

echo "MOSQUITTO_STAGE retained-messages"
# Test retained messages
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/retained -m "retained-msg" -r
sleep 1
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/retained -C 1 -W 5 > /var/lib/mosquitto/retained-sub.out 2>&1 &
sub_ret_pid=$!
wait "$sub_ret_pid" || true
grep -q 'retained-msg' /var/lib/mosquitto/retained-sub.out

echo "MOSQUITTO_STAGE persistent-session"
# Test persistent session
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/session -i "client-session" -c -q 1 -C 1 -W 5 > /var/lib/mosquitto/session-sub.out 2>&1 &
sub_sess_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/session -q 1 -m "session-msg"
wait "$sub_sess_pid" || true
grep -q 'session-msg' /var/lib/mosquitto/session-sub.out

echo "MOSQUITTO_STAGE large-payload"
# Test large payload
large_msg=$(dd if=/dev/zero bs=1024 count=10 2>/dev/null | tr '\0' 'A')
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/large -C 1 -W 5 > /var/lib/mosquitto/large-sub.out 2>&1 &
sub_large_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/large -m "$large_msg"
wait "$sub_large_pid" || true
grep -q 'AAAAAAAAAAAAAAAA' /var/lib/mosquitto/large-sub.out

echo "MOSQUITTO_STAGE multiple-clients"
# Test multiple clients
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/client1 -C 1 -W 5 > /var/lib/mosquitto/client1-sub.out 2>&1 &
sub_client1_pid=$!
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/client2 -C 1 -W 5 > /var/lib/mosquitto/client2-sub.out 2>&1 &
sub_client2_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/client1 -m "msg-for-client1"
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/client2 -m "msg-for-client2"
wait "$sub_client1_pid" || true
wait "$sub_client2_pid" || true
grep -q 'msg-for-client1' /var/lib/mosquitto/client1-sub.out
grep -q 'msg-for-client2' /var/lib/mosquitto/client2-sub.out

echo "MOSQUITTO_STAGE topic-escaping"
# Test topic escaping
mosquitto_sub -h 127.0.0.1 -p 1883 -t 'test/special/chars' -C 1 -W 5 > /var/lib/mosquitto/special-sub.out 2>&1 &
sub_special_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t 'test/special/chars' -m "special-msg"
wait "$sub_special_pid" || true
grep -q 'special-msg' /var/lib/mosquitto/special-sub.out

echo "MOSQUITTO_STAGE last-will-testament"
# Test Last Will and Testament - critical for embedded device offline detection
mosquitto_sub -h 127.0.0.1 -p 1883 -t devices/gateway/status -C 1 -W 10 > /var/lib/mosquitto/lwt-sub.out 2>&1 &
sub_lwt_pid=$!
sleep 1
# Start client with LWT; SIGKILL required to trigger will (clean disconnect won't)
mosquitto_sub -h 127.0.0.1 -p 1883 -t dummy -i "lwt-gw" --will-topic devices/gateway/status --will-payload '{"state":"offline","reason":"unexpected"}' --will-qos 1 --will-retain -W 30 > /dev/null 2>&1 &
lwt_pid=$!
sleep 2
kill -9 "$lwt_pid" 2>/dev/null || true
wait "$lwt_pid" 2>/dev/null || true
wait "$sub_lwt_pid" || true
grep -q 'offline' /var/lib/mosquitto/lwt-sub.out

echo "MOSQUITTO_STAGE authentication"
# Test username/password authentication - production embedded devices need auth
mkdir -p /var/lib/mosquitto/mosquitto
# mosquitto 2.x requires hashed password file
mosquitto_passwd -b -c /var/lib/mosquitto/mosquitto/passwd testuser testpass 2>/dev/null
chmod 644 /var/lib/mosquitto/mosquitto/passwd
cat > /var/lib/mosquitto/mosquitto-auth.conf << 'EOF'
listener 1884
socket_domain ipv4
allow_anonymous false
password_file /var/lib/mosquitto/mosquitto/passwd
persistence false
log_dest stderr
log_type error
EOF

/usr/sbin/mosquitto -c /var/lib/mosquitto/mosquitto-auth.conf > /var/lib/mosquitto/mosquitto-auth.log 2>&1 &
auth_pid=$!
sleep 2
# Test auth with correct credentials
mosquitto_sub -h 127.0.0.1 -p 1884 -t test/auth -C 1 -W 5 -u testuser -P testpass > /var/lib/mosquitto/auth-sub.out 2>&1 &
sub_auth_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1884 -t test/auth -m "auth-ok" -u testuser -P testpass
wait "$sub_auth_pid" || true
grep -q 'auth-ok' /var/lib/mosquitto/auth-sub.out
kill "$auth_pid" 2>/dev/null || true
wait "$auth_pid" 2>/dev/null || true

echo "MOSQUITTO_STAGE keepalive-reconnect"
# Test keep-alive with reconnection - embedded devices often reconnect after disconnect
mosquitto_sub -h 127.0.0.1 -p 1883 -t test/keepalive -C 2 -W 15 -k 5 > /var/lib/mosquitto/keepalive-sub.out 2>&1 &
sub_ka_pid=$!
sleep 1
# First message with keep-alive
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/keepalive -m "ka-msg1" -i "ka-pub" -k 5
sleep 6
# Second message after keep-alive interval (connection maintained by ping)
mosquitto_pub -h 127.0.0.1 -p 1883 -t test/keepalive -m "ka-msg2" -i "ka-pub" -k 5
wait "$sub_ka_pid" || true
grep -q 'ka-msg1' /var/lib/mosquitto/keepalive-sub.out
grep -q 'ka-msg2' /var/lib/mosquitto/keepalive-sub.out

echo "MOSQUITTO_STAGE device-hierarchy"
# Test device topic hierarchy - standard IoT pattern: devices/{id}/telemetry, devices/{id}/commands
mosquitto_sub -h 127.0.0.1 -p 1883 -t devices/+/telemetry -C 3 -W 5 > /var/lib/mosquitto/device-telemetry.out 2>&1 &
sub_dev_pid=$!
mosquitto_sub -h 127.0.0.1 -p 1883 -t devices/actuator01/commands -C 1 -W 5 > /var/lib/mosquitto/device-cmd.out 2>&1 &
sub_cmd_pid=$!
sleep 1
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor01/telemetry -m '{"temp":21.5}'
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor02/telemetry -m '{"temp":23.1}'
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/sensor03/telemetry -m '{"temp":19.8}'
mosquitto_pub -h 127.0.0.1 -p 1883 -t devices/actuator01/commands -m '{"action":"on"}'
wait "$sub_dev_pid" || true
wait "$sub_cmd_pid" || true
grep -q '21.5' /var/lib/mosquitto/device-telemetry.out
grep -q '23.1' /var/lib/mosquitto/device-telemetry.out
grep -q '19.8' /var/lib/mosquitto/device-telemetry.out
grep -q 'on' /var/lib/mosquitto/device-cmd.out

echo "MOSQUITTO_STAGE small-payload-burst"
# Test small payload burst - typical embedded sensor reporting pattern
# 50 tiny messages (temperature readings), simulating a sensor reporting every 200ms
burst_count=50
mosquitto_sub -h 127.0.0.1 -p 1883 -t sensors/temp -C "$burst_count" -W 120 > /var/lib/mosquitto/burst-sub.out 2>&1 &
sub_burst_pid=$!
sleep 1
i=0
while [ "$i" -lt "$burst_count" ]; do
    temp=$((2000 + i))  # 20.00 + i*0.01 degrees
    mosquitto_pub -h 127.0.0.1 -p 1883 -t sensors/temp -m "${temp}" -q 0
    i=$((i + 1))
done
wait "$sub_burst_pid" || true
# Verify first and last readings received
grep -q '2000' /var/lib/mosquitto/burst-sub.out || fail_with_file /var/lib/mosquitto/burst-sub.out
grep -q '2049' /var/lib/mosquitto/burst-sub.out || fail_with_file /var/lib/mosquitto/burst-sub.out

test_done=1
trap - EXIT
cleanup

echo "MOSQUITTO_TEST_PASSED"
