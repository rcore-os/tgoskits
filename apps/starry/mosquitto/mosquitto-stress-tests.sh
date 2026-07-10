#!/bin/sh
set -eu

mosquitto_pid=""
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
connection_messages true
max_inflight_messages 100
max_queued_messages 10000
EOF

    /usr/sbin/mosquitto -c "$config_file" > /var/lib/mosquitto/mosquitto-stress.log 2>&1 &
    mosquitto_pid=$!

    i=0
    while [ "$i" -lt 30 ]; do
        if mosquitto_pub -h 127.0.0.1 -p 1883 -t test/ping -m "ping" 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    echo "=== mosquitto-stress.log ==="
    cat /var/lib/mosquitto/mosquitto-stress.log
    echo "MOSQUITTO_STRESS_TEST_FAILED"
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
        echo "MOSQUITTO_STRESS_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail_with_file() {
    path="$1"
    echo "=== $path ==="
    cat "$path" 2>/dev/null || true
    echo "MOSQUITTO_STRESS_TEST_FAILED"
    exit 1
}

run_publisher() {
    client_id="$1"
    topic="$2"
    count="$3"
    qos="$4"

    i=0
    while [ "$i" -lt "$count" ]; do
        mosquitto_pub -h 127.0.0.1 -p 1883 -t "$topic" -m "stress-$client_id-$i" -q "$qos" -i "pub-$client_id"
        i=$((i + 1))
    done
}

run_subscriber() {
    client_id="$1"
    topic="$2"
    count="$3"
    qos="$4"

    mosquitto_sub -h 127.0.0.1 -p 1883 -t "$topic" -C "$count" -W 120 -q "$qos" -i "sub-$client_id" > "/var/lib/mosquitto/stress-sub-$client_id.out" 2>&1
}

echo "MOSQUITTO_STRESS_STAGE start"
start_mosquitto /var/lib/mosquitto/mosquitto-stress.conf

stress_rounds=2
stress_ops=50

round=0
while [ "$round" -lt "$stress_rounds" ]; do
    echo "MOSQUITTO_STRESS_STAGE round-$round-basic"
    # Basic stress test with multiple messages
    run_subscriber "basic-$round" "stress/basic/$round" "$stress_ops" 0 &
    sub_basic_pid=$!
    sleep 1
    run_publisher "basic-$round" "stress/basic/$round" "$stress_ops" 0
    wait "$sub_basic_pid" || fail_with_file "/var/lib/mosquitto/stress-sub-basic-$round.out"

    echo "MOSQUITTO_STRESS_STAGE round-$round-qos1"
    # QoS 1 stress test
    run_subscriber "qos1-$round" "stress/qos1/$round" "$stress_ops" 1 &
    sub_qos1_pid=$!
    sleep 1
    run_publisher "qos1-$round" "stress/qos1/$round" "$stress_ops" 1
    wait "$sub_qos1_pid" || fail_with_file "/var/lib/mosquitto/stress-sub-qos1-$round.out"

    echo "MOSQUITTO_STRESS_STAGE round-$round-multi-topic"
    # Multiple topics stress test
    topic_count=5
    msgs_per_topic=10
    total_msgs=$((topic_count * msgs_per_topic))
    mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/multi/$round/#" -C "$total_msgs" -W 120 -i "sub-multi-$round" > "/var/lib/mosquitto/stress-multi-$round.out" 2>&1 &
    sub_multi_pid=$!
    sleep 1
    t=0
    while [ "$t" -lt "$topic_count" ]; do
        m=0
        while [ "$m" -lt "$msgs_per_topic" ]; do
            mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/multi/$round/topic$t" -m "msg-$t-$m" -i "pub-multi-$round-$t"
            m=$((m + 1))
        done
        t=$((t + 1))
    done
    wait "$sub_multi_pid" || fail_with_file "/var/lib/mosquitto/stress-multi-$round.out"

    round=$((round + 1))
done

echo "MOSQUITTO_STRESS_STAGE high-frequency"
# High frequency publish/subscribe
high_freq_count=100
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/highfreq" -C "$high_freq_count" -W 120 -i "sub-highfreq" > "/var/lib/mosquitto/stress-highfreq.out" 2>&1 &
sub_hf_pid=$!
sleep 1
i=0
while [ "$i" -lt "$high_freq_count" ]; do
    mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/highfreq" -m "hf-$i" -i "pub-highfreq"
    i=$((i + 1))
done
wait "$sub_hf_pid" || fail_with_file "/var/lib/mosquitto/stress-highfreq.out"

echo "MOSQUITTO_STRESS_STAGE concurrent-clients"
# Concurrent client stress test
concurrent_clients=5
msgs_per_client=20
total_concurrent_msgs=$((concurrent_clients * msgs_per_client))
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/concurrent/#" -C "$total_concurrent_msgs" -W 120 -i "sub-concurrent" > "/var/lib/mosquitto/stress-concurrent.out" 2>&1 &
sub_conc_pid=$!
sleep 1
conc_pids=""
c=0
while [ "$c" -lt "$concurrent_clients" ]; do
    (
        m=0
        while [ "$m" -lt "$msgs_per_client" ]; do
            mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/concurrent/client$c" -m "conc-$c-$m" -i "pub-conc-$c"
            m=$((m + 1))
        done
    ) &
    conc_pids="$conc_pids $!"
    c=$((c + 1))
done
for pid in $conc_pids; do
    wait "$pid" || true
done
wait "$sub_conc_pid" || fail_with_file "/var/lib/mosquitto/stress-concurrent.out"

echo "MOSQUITTO_STRESS_STAGE large-messages"
# Large message stress test
large_msg=$(dd if=/dev/zero bs=4096 count=10 2>/dev/null | tr '\0' 'B')
large_count=10
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/large" -C "$large_count" -W 120 -i "sub-large" > "/var/lib/mosquitto/stress-large.out" 2>&1 &
sub_large_pid=$!
sleep 1
i=0
while [ "$i" -lt "$large_count" ]; do
    mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/large" -m "$large_msg" -i "pub-large"
    i=$((i + 1))
done
wait "$sub_large_pid" || fail_with_file "/var/lib/mosquitto/stress-large.out"

echo "MOSQUITTO_STRESS_STAGE wildcard-stress"
# Wildcard subscription stress test
wild_topics=10
wild_msgs_per_topic=5
wild_total=$((wild_topics * wild_msgs_per_topic))
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/wild/#" -C "$wild_total" -W 120 -i "sub-wild" > "/var/lib/mosquitto/stress-wild.out" 2>&1 &
sub_wild_pid=$!
sleep 1
t=0
while [ "$t" -lt "$wild_topics" ]; do
    m=0
    while [ "$m" -lt "$wild_msgs_per_topic" ]; do
        mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/wild/topic$t" -m "wild-$t-$m" -i "pub-wild-$t"
        m=$((m + 1))
    done
    t=$((t + 1))
done
wait "$sub_wild_pid" || fail_with_file "/var/lib/mosquitto/stress-wild.out"

echo "MOSQUITTO_STRESS_STAGE mixed-qos"
# Mixed QoS stress test
mixed_count=30
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/mixed" -C "$mixed_count" -W 120 -i "sub-mixed" > "/var/lib/mosquitto/stress-mixed.out" 2>&1 &
sub_mixed_pid=$!
sleep 1
i=0
while [ "$i" -lt "$mixed_count" ]; do
    qos=$((i % 3))
    mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/mixed" -q "$qos" -m "mixed-qos$qos-$i" -i "pub-mixed"
    i=$((i + 1))
done
wait "$sub_mixed_pid" || fail_with_file "/var/lib/mosquitto/stress-mixed.out"

echo "MOSQUITTO_STRESS_STAGE persistence-restart"
# Test persistence across restart
mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/persist" -m "persist-value" -r -i "pub-persist"
sleep 2
stop_mosquitto
start_mosquitto /var/lib/mosquitto/mosquitto-stress.conf
sleep 2
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/persist" -C 1 -W 10 -i "sub-persist" > "/var/lib/mosquitto/stress-persist.out" 2>&1 &
sub_persist_pid=$!
wait "$sub_persist_pid" || fail_with_file "/var/lib/mosquitto/stress-persist.out"
grep -q 'persist-value' /var/lib/mosquitto/stress-persist.out

echo "MOSQUITTO_STRESS_STAGE embedded-burst"
# Embedded sensor burst stress - many devices sending small readings simultaneously
embedded_count=20
embedded_msgs_per_device=10
embedded_total=$((embedded_count * embedded_msgs_per_device))
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/embedded/+/telemetry" -C "$embedded_total" -W 120 -i "sub-embedded" > "/var/lib/mosquitto/stress-embedded.out" 2>&1 &
sub_embed_pid=$!
sleep 1
embed_pids=""
d=0
while [ "$d" -lt "$embedded_count" ]; do
    (
        m=0
        while [ "$m" -lt "$embedded_msgs_per_device" ]; do
            temp=$((2000 + m * 5))
            mosquitto_pub -h 127.0.0.1 -p 1883 -t "stress/embedded/dev$d/telemetry" -m "{\"dev\":$d,\"temp\":$temp}" -i "pub-embedded-$d" -q 0
            m=$((m + 1))
        done
    ) &
    embed_pids="$embed_pids $!"
    d=$((d + 1))
done
for pid in $embed_pids; do
    wait "$pid" || true
done
wait "$sub_embed_pid" || fail_with_file "/var/lib/mosquitto/stress-embedded.out"

echo "MOSQUITTO_STRESS_STAGE lwt-storm"
# LWT storm - simulate many devices going offline simultaneously
lwt_storm_count=10
mosquitto_sub -h 127.0.0.1 -p 1883 -t "stress/lwt/status" -C "$lwt_storm_count" -W 120 -q 1 -i "sub-lwt-storm" > "/var/lib/mosquitto/stress-lwt-storm.out" 2>&1 &
sub_lwt_pid=$!
sleep 1
# Start many clients with LWT messages
lwt_pids=""
d=0
while [ "$d" -lt "$lwt_storm_count" ]; do
    mosquitto_sub -h 127.0.0.1 -p 1883 -t dummy -i "lwt-dev-$d" --will-topic "stress/lwt/status" --will-payload "dev-$d-offline" --will-qos 1 -W 120 > /dev/null 2>&1 &
    lwt_pids="$lwt_pids $!"
    d=$((d + 1))
done
sleep 5
# SIGKILL all LWT clients to trigger will messages (clean disconnect won't trigger LWT)
for pid in $lwt_pids; do
    kill -9 "$pid" 2>/dev/null || true
done
for pid in $lwt_pids; do
    wait "$pid" 2>/dev/null || true
done
wait "$sub_lwt_pid" || fail_with_file "/var/lib/mosquitto/stress-lwt-storm.out"

test_done=1
trap - EXIT
cleanup

echo "MOSQUITTO_STRESS_TEST_PASSED"
