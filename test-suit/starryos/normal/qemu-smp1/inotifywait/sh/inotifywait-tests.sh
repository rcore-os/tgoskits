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
out="$workdir/out"
err="$workdir/err"

rm -rf "$workdir"
mkdir -p "$workdir"
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

echo "INOTIFYWAIT_TEST_PASSED"
