#!/bin/sh
set -eu

redis_pid=""
test_done=0

fail() {
    if [ -s /tmp/redis-aof-appendonly.log ]; then
        echo "=== redis-aof-appendonly.log ==="
        cat /tmp/redis-aof-appendonly.log
    fi
    echo "REDIS_AOF_APPENDONLY_TEST_FAILED"
    exit 1
}

stop_redis() {
    if [ -n "$redis_pid" ]; then
        redis-cli -h 127.0.0.1 -p 6379 SHUTDOWN NOSAVE >/dev/null 2>&1 || true
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

on_exit() {
    rc=$?
    stop_redis
    if [ "$test_done" -ne 1 ]; then
        echo "REDIS_AOF_APPENDONLY_TEST_FAILED"
    fi
    exit "$rc"
}
trap on_exit EXIT

rm -rf /tmp/redis-aof-appendonly
mkdir -p /tmp/redis-aof-appendonly

redis-server \
    --port 6379 \
    --bind 127.0.0.1 \
    --protected-mode no \
    --daemonize no \
    --dir /tmp/redis-aof-appendonly \
    --save "" \
    --appendonly yes \
    --appendfilename appendonly.aof \
    --appendfsync always \
    --logfile "" \
    --ignore-warnings ARM64-COW-BUG \
    > /tmp/redis-aof-appendonly.log 2>&1 &
redis_pid=$!

i=0
while [ "$i" -lt 60 ]; do
    if redis-cli -h 127.0.0.1 -p 6379 PING 2>/dev/null | grep -q '^PONG$'; then
        redis-cli -h 127.0.0.1 -p 6379 SET aof:appendonly ok | grep -q '^OK$' || fail
        stop_redis
        test_done=1
        trap - EXIT
        echo "REDIS_AOF_APPENDONLY_TEST_PASSED"
        exit 0
    fi
    i=$((i + 1))
    sleep 1
done

fail
