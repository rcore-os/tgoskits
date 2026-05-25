#!/bin/sh
set -eu

fail() {
    echo "INOTIFYWAIT_TEST_FAILED: $*"
    exit 1
}

apk update || fail "apk update"
apk add inotify-tools || fail "apk add inotify-tools"

command -v inotifywait >/dev/null || fail "inotifywait missing"

workdir=/tmp/starry-inotifywait
watched="$workdir/watched.txt"
watchdir="$workdir/watchdir"
out="$workdir/out"
err="$workdir/err"

rm -rf "$workdir"
mkdir -p "$workdir"
mkdir -p "$watchdir"
: > "$watched"

(
    sleep 1
    echo changed >> "$watched"
) &
writer=$!

if ! inotifywait -q -e modify --format "%w %e" -t 5 "$watched" > "$out" 2> "$err"; then
    wait "$writer" || true
    cat "$err"
    fail "inotifywait did not observe modify"
fi

wait "$writer" || true
grep -q "MODIFY" "$out" || fail "missing MODIFY event"

(
    sleep 1
    : > "$watchdir/created.txt"
) &
creator=$!

if ! inotifywait -q -e create --format "%f %e" -t 5 "$watchdir" > "$out" 2> "$err"; then
    wait "$creator" || true
    cat "$err"
    fail "inotifywait did not observe create"
fi

wait "$creator" || true
grep -q "created.txt .*CREATE" "$out" || fail "missing CREATE event"

(
    sleep 1
    echo closed > "$watchdir/closed.txt"
) &
closer=$!

if ! inotifywait -q -e close_write --format "%f %e" -t 5 "$watchdir" > "$out" 2> "$err"; then
    wait "$closer" || true
    cat "$err"
    fail "inotifywait did not observe close_write"
fi

wait "$closer" || true
grep -q "closed.txt .*CLOSE_WRITE" "$out" || fail "missing CLOSE_WRITE event"

: > "$watchdir/deleted.txt"
(
    sleep 1
    rm "$watchdir/deleted.txt"
) &
deleter=$!

if ! inotifywait -q -e delete --format "%f %e" -t 5 "$watchdir" > "$out" 2> "$err"; then
    wait "$deleter" || true
    cat "$err"
    fail "inotifywait did not observe delete"
fi

wait "$deleter" || true
grep -q "deleted.txt .*DELETE" "$out" || fail "missing DELETE event"

echo "INOTIFYWAIT_TEST_PASSED"
