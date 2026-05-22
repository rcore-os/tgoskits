#!/bin/sh
set -eu

redis_pid=""
test_done=0

start_redis() {
    redis_dir="$1"
    appendonly="$2"

    mkdir -p "$redis_dir"
    cat > /tmp/redis-stress.conf <<EOF
bind 127.0.0.1
port 6379
protected-mode no
daemonize no
supervised no
dir $redis_dir
save ""
appendonly $appendonly
appendfsync always
logfile ""
ignore-warnings ARM64-COW-BUG
EOF

    redis-server /tmp/redis-stress.conf > /tmp/redis-stress.log 2>&1 &
    redis_pid=$!

    i=0
    while [ "$i" -lt 60 ]; do
        if redis-cli -h 127.0.0.1 -p 6379 PING 2>/dev/null | grep -q '^PONG$'; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    echo "=== redis-stress.log ==="
    cat /tmp/redis-stress.log
    echo "REDIS_STRESS_TEST_FAILED"
    exit 1
}

stop_redis() {
    mode="$1"

    if [ -n "$redis_pid" ]; then
        redis-cli -h 127.0.0.1 -p 6379 SHUTDOWN "$mode" >/dev/null 2>&1 || true
        i=0
        while kill -0 "$redis_pid" >/dev/null 2>&1 && [ "$i" -lt 30 ]; do
            i=$((i + 1))
            sleep 1
        done
        kill "$redis_pid" >/dev/null 2>&1 || true
        wait "$redis_pid" >/dev/null 2>&1 || true
        redis_pid=""
    fi
}

kill_redis_hard() {
    if [ -n "$redis_pid" ]; then
        kill -9 "$redis_pid" >/dev/null 2>&1 || true
        wait "$redis_pid" >/dev/null 2>&1 || true
        redis_pid=""
    fi
}

cleanup() {
    stop_redis NOSAVE
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "REDIS_STRESS_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail_with_file() {
    path="$1"
    echo "=== $path ==="
    cat "$path" 2>/dev/null || true
    echo "REDIS_STRESS_TEST_FAILED"
    exit 1
}

wait_for_aof_rewrite() {
    i=0
    while [ "$i" -lt 90 ]; do
        if redis-cli -h 127.0.0.1 -p 6379 INFO persistence > /tmp/aof-stress-info.out 2>&1 &&
            grep -q '^aof_rewrite_in_progress:0' /tmp/aof-stress-info.out; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    fail_with_file /tmp/aof-stress-info.out
}

wait_for_bgsave() {
    i=0
    while [ "$i" -lt 90 ]; do
        if redis-cli -h 127.0.0.1 -p 6379 LASTSAVE >/dev/null 2>&1; then
            if redis-cli -h 127.0.0.1 -p 6379 INFO persistence 2>/dev/null | grep -q '^rdb_last_bgsave_status:ok'; then
                return 0
            fi
        fi
        i=$((i + 1))
        sleep 1
    done
    fail_with_file /tmp/redis-stress.log
}

run_pipeline_client() {
    client="$1"
    rounds="$2"

    i=0
    while [ "$i" -lt "$rounds" ]; do
        printf 'INCR stress:counter\r\n'
        printf 'SET stress:%s:%d value-%s-%d\r\n' "$client" "$i" "$client" "$i"
        i=$((i + 1))
    done | timeout 60 redis-cli -h 127.0.0.1 -p 6379 --pipe > "/tmp/stress-client-$client.out" 2>&1
}

echo "REDIS_STRESS_STAGE start"
start_redis /tmp/redis-stress yes

stress_rounds=4
stress_ops=32

round=0
while [ "$round" -lt "$stress_rounds" ]; do
    echo "REDIS_STRESS_STAGE round-$round-pipeline"
    run_pipeline_client 1 "$stress_ops"
    run_pipeline_client 2 "$stress_ops"
    run_pipeline_client 3 "$stress_ops"
    run_pipeline_client 4 "$stress_ops"
    grep -q 'errors: 0' /tmp/stress-client-1.out || fail_with_file /tmp/stress-client-1.out
    grep -q 'errors: 0' /tmp/stress-client-2.out || fail_with_file /tmp/stress-client-2.out
    grep -q 'errors: 0' /tmp/stress-client-3.out || fail_with_file /tmp/stress-client-3.out
    grep -q 'errors: 0' /tmp/stress-client-4.out || fail_with_file /tmp/stress-client-4.out

    expected=$(((round + 1) * 4 * stress_ops))
    test "$(redis-cli -h 127.0.0.1 -p 6379 GET stress:counter)" = "$expected"

    echo "REDIS_STRESS_STAGE round-$round-blocking"
    timeout 8 redis-cli -h 127.0.0.1 -p 6379 BLPOP "stress:list:$round" 5 > /tmp/stress-blpop.out 2>&1 &
    blpop_pid=$!
    sleep 1
    test "$(redis-cli -h 127.0.0.1 -p 6379 RPUSH "stress:list:$round" "payload-$round")" = "1"
    wait "$blpop_pid" || true
    grep -q "payload-$round" /tmp/stress-blpop.out || fail_with_file /tmp/stress-blpop.out

    if [ $((round % 2)) -eq 0 ]; then
        echo "REDIS_STRESS_STAGE round-$round-bgsave"
        redis-cli -h 127.0.0.1 -p 6379 BGSAVE | grep -q 'Background saving started'
        wait_for_bgsave
    else
        echo "REDIS_STRESS_STAGE round-$round-aof-rewrite"
        redis-cli -h 127.0.0.1 -p 6379 BGREWRITEAOF | grep -q 'Background append only file rewriting started'
        wait_for_aof_rewrite
    fi

    round=$((round + 1))
done

echo "REDIS_STRESS_STAGE large-dataset-bgsave"
i=0
while [ "$i" -lt 256 ]; do
    printf 'SET large:%d %0256d\r\n' "$i" "$i"
    i=$((i + 1))
done | timeout 60 redis-cli -h 127.0.0.1 -p 6379 --pipe > /tmp/large-pipe.out 2>&1
grep -q 'errors: 0' /tmp/large-pipe.out || fail_with_file /tmp/large-pipe.out
test "$(redis-cli -h 127.0.0.1 -p 6379 GET large:255)" != ""

redis-cli -h 127.0.0.1 -p 6379 BGSAVE | grep -q 'Background saving started'
wait_for_bgsave

echo "REDIS_STRESS_STAGE connect-churn"
j=0
while [ "$j" -lt 3 ]; do
    i=0
    while [ "$i" -lt 8 ]; do
        (
            k=0
            while [ "$k" -lt 16 ]; do
                printf 'SET storm:%d:%d:%d v%d\r\n' "$j" "$i" "$k" "$k"
                k=$((k + 1))
            done
        ) | timeout 30 redis-cli -h 127.0.0.1 -p 6379 --pipe > "/tmp/storm-$j-$i.out" 2>&1
        i=$((i + 1))
    done

    all_ok=1
    i=0
    while [ "$i" -lt 8 ]; do
        if ! grep -q 'errors: 0' "/tmp/storm-$j-$i.out"; then
            echo "WARN: storm client $j-$i had errors"
            all_ok=0
        fi
        i=$((i + 1))
    done
    test "$all_ok" = "1"
    j=$((j + 1))
done

test "$(redis-cli -h 127.0.0.1 -p 6379 GET storm:2:7:15)" = "v15"

redis-cli -h 127.0.0.1 -p 6379 BGREWRITEAOF | grep -q 'Background append only file rewriting started'
wait_for_aof_rewrite

echo "REDIS_STRESS_STAGE hard-kill-recovery"
redis-cli -h 127.0.0.1 -p 6379 SET crash:key crash-value | grep -q '^OK$'
kill_redis_hard
start_redis /tmp/redis-stress yes
test "$(redis-cli -h 127.0.0.1 -p 6379 GET crash:key)" = "crash-value"

echo "REDIS_STRESS_STAGE restart-readback"
stop_redis SAVE
start_redis /tmp/redis-stress yes
last_op=$((stress_ops - 1))
test "$(redis-cli -h 127.0.0.1 -p 6379 GET stress:counter)" = "$((stress_rounds * 4 * stress_ops))"
test "$(redis-cli -h 127.0.0.1 -p 6379 GET "stress:4:$last_op")" = "value-4-$last_op"
test "$(redis-cli -h 127.0.0.1 -p 6379 GET large:255)" != ""
stop_redis NOSAVE

test_done=1
trap - EXIT

echo "REDIS_STRESS_TEST_PASSED"
