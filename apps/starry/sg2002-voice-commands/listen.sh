#!/bin/sh
# Run beside run.sh in the deployment directory. Keep recognition and ALSA
# failures visible instead of allowing a successful pipeline tail to hide them.
set -eu
app_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
temporary=$(mktemp -d)
capture_pid=
decoder_pid=
cleanup() {
    saved_status=$?
    trap - EXIT
    trap '' INT TERM
    # Notify both owners before waiting; allow at most two seconds to stop.
    for pid in "$capture_pid" "$decoder_pid"; do
        [ -z "$pid" ] || kill "$pid" 2>/dev/null || true
    done
    for _ in 1 2; do
        alive=0
        for pid in "$capture_pid" "$decoder_pid"; do
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then alive=1; fi
        done
        [ "$alive" -eq 1 ] || break
        sleep 1
    done
    for pid in "$capture_pid" "$decoder_pid"; do
        [ -z "$pid" ] || kill -KILL "$pid" 2>/dev/null || true
    done
    for pid in "$capture_pid" "$decoder_pid"; do
        [ -z "$pid" ] || wait "$pid" 2>/dev/null || true
    done
    rm -f "$temporary/pcm" || true
    rmdir "$temporary" || true
    exit "$saved_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
listen() {
    mkfifo "$temporary/pcm"
    arecord -q --fatal-errors -D "${1:-hw:0,0}" -t raw -f S16_LE -c 1 -r 16000 \
        --buffer-size=8192 --period-size=1024 > "$temporary/pcm" &
    capture_pid=$!
    status=0
    "$app_dir/run.sh" --raw-file "$temporary/pcm" &
    decoder_pid=$!
    wait "$decoder_pid" || status=$?
    decoder_pid=
    if [ "$status" -ne 0 ]; then
        return "$status"
    fi
    wait "$capture_pid" || status=$?
    capture_pid=
    return "$status"
}
listen "$@"
