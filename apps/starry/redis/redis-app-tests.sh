#!/bin/sh
set -eu

redis_pid=""
test_done=0

start_redis() {
    redis_dir="$1"
    appendonly="$2"

    mkdir -p "$redis_dir"
    cat > /tmp/redis.conf <<EOF
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

    redis-server /tmp/redis.conf > /tmp/redis.log 2>&1 &
    redis_pid=$!

    i=0
    while [ "$i" -lt 60 ]; do
        if redis-cli -h 127.0.0.1 -p 6379 PING 2>/dev/null | grep -q '^PONG$'; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    echo "=== redis.log ==="
    cat /tmp/redis.log
    echo "REDIS_APP_TEST_FAILED"
    exit 1
}

stop_redis() {
    mode="$1"

    if [ -n "$redis_pid" ]; then
        redis-cli -h 127.0.0.1 -p 6379 SHUTDOWN "$mode" >/dev/null 2>&1 || true
        i=0
        while kill -0 "$redis_pid" >/dev/null 2>&1 && [ "$i" -lt 20 ]; do
            i=$((i + 1))
            sleep 1
        done
        kill "$redis_pid" >/dev/null 2>&1 || true
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
        echo "REDIS_APP_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail_with_file() {
    path="$1"
    echo "=== $path ==="
    cat "$path" 2>/dev/null || true
    echo "REDIS_APP_TEST_FAILED"
    exit 1
}

echo "REDIS_STAGE basic"
start_redis /tmp/redis-main no

redis-cli -h 127.0.0.1 -p 6379 SET starry redis | grep -q '^OK$'
test "$(redis-cli -h 127.0.0.1 -p 6379 GET starry)" = "redis"
test "$(redis-cli -h 127.0.0.1 -p 6379 INCR counter)" = "1"
test "$(redis-cli -h 127.0.0.1 -p 6379 INCR counter)" = "2"
test "$(redis-cli -h 127.0.0.1 -p 6379 LPUSH list one two three)" = "3"
test "$(redis-cli -h 127.0.0.1 -p 6379 LRANGE list 0 -1 | tr '\n' ' ')" = "three two one "
test "$(redis-cli -h 127.0.0.1 -p 6379 HSET hash field value)" = "1"
test "$(redis-cli -h 127.0.0.1 -p 6379 HGET hash field)" = "value"

echo "REDIS_STAGE multi-client"
redis-cli -h 127.0.0.1 -p 6379 SET client:1 one >/tmp/client1.out 2>&1 &
client1_pid=$!
redis-cli -h 127.0.0.1 -p 6379 SET client:2 two >/tmp/client2.out 2>&1 &
client2_pid=$!
wait "$client1_pid" || true
wait "$client2_pid" || true
grep -q '^OK$' /tmp/client1.out || fail_with_file /tmp/client1.out
grep -q '^OK$' /tmp/client2.out || fail_with_file /tmp/client2.out
redis-cli -h 127.0.0.1 -p 6379 MGET client:1 client:2 | tr '\n' ' ' > /tmp/mget-clients.out
grep -q '^one two $' /tmp/mget-clients.out || fail_with_file /tmp/mget-clients.out

echo "REDIS_STAGE pipeline"
{
    printf 'SET pipe:1 alpha\r\n'
    printf 'SET pipe:2 beta\r\n'
    printf 'INCR pipe:counter\r\n'
    printf 'INCR pipe:counter\r\n'
} | redis-cli -h 127.0.0.1 -p 6379 --pipe > /tmp/pipe.out 2>&1
grep -q 'errors: 0' /tmp/pipe.out
test "$(redis-cli -h 127.0.0.1 -p 6379 MGET pipe:1 pipe:2 pipe:counter | tr '\n' ' ')" = "alpha beta 2 "

echo "REDIS_STAGE bulk-pipeline"
i=0
while [ "$i" -lt 128 ]; do
    printf 'SET bulk:%d value-%d\r\n' "$i" "$i"
    i=$((i + 1))
done | redis-cli -h 127.0.0.1 -p 6379 --pipe > /tmp/bulk-pipe.out 2>&1
grep -q 'errors: 0' /tmp/bulk-pipe.out || fail_with_file /tmp/bulk-pipe.out
test "$(redis-cli -h 127.0.0.1 -p 6379 GET bulk:127)" = "value-127"

echo "REDIS_STAGE blocking-timeout"
timeout 5 redis-cli -h 127.0.0.1 -p 6379 BLPOP missing-list 1 > /tmp/blpop.out 2>&1 || fail_with_file /tmp/blpop.out
tr -d ' \t\r\n' < /tmp/blpop.out > /tmp/blpop.trimmed
test ! -s /tmp/blpop.trimmed || fail_with_file /tmp/blpop.out

echo "REDIS_STAGE blocking-wakeup"
timeout 8 redis-cli -h 127.0.0.1 -p 6379 BLPOP ready-list 5 > /tmp/blpop-ready.out 2>&1 &
blpop_ready_pid=$!
sleep 1
test "$(redis-cli -h 127.0.0.1 -p 6379 RPUSH ready-list payload)" = "1"
wait "$blpop_ready_pid" || fail_with_file /tmp/blpop-ready.out
grep -q 'ready-list' /tmp/blpop-ready.out || fail_with_file /tmp/blpop-ready.out
grep -q 'payload' /tmp/blpop-ready.out || fail_with_file /tmp/blpop-ready.out

echo "REDIS_STAGE pubsub"
timeout 5 redis-cli -h 127.0.0.1 -p 6379 SUBSCRIBE starry-channel > /tmp/sub.out 2>&1 &
sub_pid=$!
sleep 1
test "$(redis-cli -h 127.0.0.1 -p 6379 PUBLISH starry-channel hello)" = "1"
sleep 1
kill "$sub_pid" >/dev/null 2>&1 || true
wait "$sub_pid" >/dev/null 2>&1 || true
grep -q 'starry-channel' /tmp/sub.out
grep -q 'hello' /tmp/sub.out

echo "REDIS_STAGE rdb-save"
redis-cli -h 127.0.0.1 -p 6379 SET persist:rdb value-rdb | grep -q '^OK$'
redis-cli -h 127.0.0.1 -p 6379 SAVE | grep -q '^OK$'
test -s /tmp/redis-main/dump.rdb
stop_redis SAVE

echo "REDIS_STAGE rdb-restart"
start_redis /tmp/redis-main no
test "$(redis-cli -h 127.0.0.1 -p 6379 GET persist:rdb)" = "value-rdb"
redis-cli -h 127.0.0.1 -p 6379 SET persist:bgsave value-bgsave | grep -q '^OK$'
redis-cli -h 127.0.0.1 -p 6379 BGSAVE | grep -q 'Background saving started'
i=0
while [ "$i" -lt 60 ]; do
    if redis-cli -h 127.0.0.1 -p 6379 LASTSAVE >/tmp/lastsave.after 2>&1 && test -s /tmp/redis-main/dump.rdb; then
        break
    fi
    i=$((i + 1))
    sleep 1
done
test "$i" -lt 60
stop_redis SAVE

echo "REDIS_STAGE bgsave-restart"
start_redis /tmp/redis-main no
test "$(redis-cli -h 127.0.0.1 -p 6379 GET persist:rdb)" = "value-rdb"
test "$(redis-cli -h 127.0.0.1 -p 6379 GET persist:bgsave)" = "value-bgsave"
stop_redis NOSAVE

echo "REDIS_STAGE aof-save"
start_redis /tmp/redis-aof yes
redis-cli -h 127.0.0.1 -p 6379 SET persist:aof value-aof | grep -q '^OK$'
stop_redis SAVE

echo "REDIS_STAGE aof-restart"
start_redis /tmp/redis-aof yes
test "$(redis-cli -h 127.0.0.1 -p 6379 GET persist:aof)" = "value-aof"

echo "REDIS_STAGE aof-rewrite"
i=0
while [ "$i" -lt 64 ]; do
    redis-cli -h 127.0.0.1 -p 6379 SET "rewrite:$i" "value-$i" >/dev/null
    i=$((i + 1))
done
redis-cli -h 127.0.0.1 -p 6379 BGREWRITEAOF | grep -q 'Background append only file rewriting started'
i=0
while [ "$i" -lt 60 ]; do
    if redis-cli -h 127.0.0.1 -p 6379 INFO persistence > /tmp/aof-info.out 2>&1 &&
        grep -q '^aof_rewrite_in_progress:0' /tmp/aof-info.out; then
        break
    fi
    i=$((i + 1))
    sleep 1
done
test "$i" -lt 60 || fail_with_file /tmp/aof-info.out
test "$(redis-cli -h 127.0.0.1 -p 6379 GET rewrite:63)" = "value-63"
stop_redis NOSAVE

test_done=1
trap - EXIT

echo "REDIS_APP_TEST_PASSED"
