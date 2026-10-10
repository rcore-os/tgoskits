#!/bin/sh
set -u

server_ip=$1
duration=$2
warmup=$3
marker=$4
base_url="http://${server_ip}:3000/v1/tests"
work_dir=$(mktemp -d) || exit 1
upload_pid=
download_pid=

cleanup() {
    [ -z "$upload_pid" ] || kill "$upload_pid" 2>/dev/null || true
    [ -z "$download_pid" ] || kill "$download_pid" 2>/dev/null || true
    rm -rf "$work_dir"
}
trap cleanup EXIT

fail() {
    printf '%s_FAILED\n' "$marker"
    exit 1
}

direction_field() {
    printf '%s\n' "$record" | sed -n "s/.*\"$1\":{[^}]*\"$2\":\"\([^\"]*\)\".*/\1/p"
}

direction_bytes() {
    printf '%s\n' "$record" | sed -n "s/.*\"$1\":{[^}]*\"bytes\":\([0-9][0-9]*\).*/\1/p"
}

direction_elapsed() {
    printf '%s\n' "$record" | sed -n "s/.*\"$1\":{[^}]*\"elapsed_ms\":\([0-9][0-9]*\).*/\1/p"
}

command -v curl >/dev/null 2>&1 || fail
command -v upload-source >/dev/null 2>&1 || fail
test_id=$(curl -fsS --connect-timeout 2 --max-time 5 -X POST "$base_url") || fail
test_id=$(printf '%s\n' "$test_id" | sed -n 's/.*"test_id":"\([a-f0-9-][a-f0-9-]*\)".*/\1/p')
case "$test_id" in
    ''|*[!a-f0-9-]*) fail ;;
esac
test_url="$base_url/$test_id"
printf 'network test id=%s server=%s:3000 duration=%ss\n' "$test_id" "$server_ip" "$duration"

(upload-source "$duration" | curl -fsS --connect-timeout 2 --max-time "$((duration + 30))" \
    --upload-file - "$test_url/upload" >"$work_dir/upload.json") \
    2>"$work_dir/upload.log" &
upload_pid=$!
curl -fsS --connect-timeout 2 --max-time "$((duration + 30))" \
    "$test_url/download?duration_secs=$duration" -o /dev/null \
    -w '%{size_download}' >"$work_dir/download.bytes" \
    2>"$work_dir/download.log" &
download_pid=$!

elapsed=0
last_upload=0
last_download=0
upload_stall=0
download_stall=0
upload_progressed=0
download_progressed=0
while [ "$elapsed" -le "$((duration + 20))" ]; do
    record=$(curl -fsS --connect-timeout 2 --max-time 5 "$test_url") || fail
    upload_status=$(direction_field upload status)
    download_status=$(direction_field download status)
    upload_bytes=$(direction_bytes upload)
    download_bytes=$(direction_bytes download)
    [ -n "$upload_status" ] && [ -n "$download_status" ] || fail
    [ -n "$upload_bytes" ] && [ -n "$download_bytes" ] || fail
    printf 'second=%s upload=%s:%s download=%s:%s\n' \
        "$elapsed" "$upload_status" "$upload_bytes" "$download_status" "$download_bytes"
    case "$upload_status:$download_status" in
        *failed*|*canceled*|*timed_out*) fail ;;
    esac
    if [ "$elapsed" -ge "$warmup" ]; then
        if [ "$upload_status" = running ] && [ "$upload_bytes" -gt "$last_upload" ]; then
            upload_progressed=1
        fi
        if [ "$download_status" = running ] && [ "$download_bytes" -gt "$last_download" ]; then
            download_progressed=1
        fi
        if [ "$upload_status" = running ] && [ "$upload_bytes" -le "$last_upload" ]; then
            upload_stall=$((upload_stall + 1))
        else
            upload_stall=0
        fi
        if [ "$download_status" = running ] && [ "$download_bytes" -le "$last_download" ]; then
            download_stall=$((download_stall + 1))
        else
            download_stall=0
        fi
        [ "$upload_stall" -lt 3 ] && [ "$download_stall" -lt 3 ] || fail
    fi
    last_upload=$upload_bytes
    last_download=$download_bytes
    if [ "$upload_status" = completed ] && [ "$download_status" = completed ]; then
        break
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done

wait "$upload_pid" || { cat "$work_dir/upload.log"; fail; }
upload_pid=
wait "$download_pid" || { cat "$work_dir/download.log"; fail; }
download_pid=
[ "$upload_status" = completed ] && [ "$download_status" = completed ] || fail
[ "$upload_bytes" -gt 0 ] && [ "$download_bytes" -gt 0 ] || fail
[ "$upload_progressed" = 1 ] && [ "$download_progressed" = 1 ] || fail
source_bytes=$(sed -n 's/^BYTES=\([0-9][0-9]*\)$/\1/p' "$work_dir/upload.log")
source_elapsed=$(sed -n 's/^ELAPSED_MS=\([0-9][0-9]*\)$/\1/p' "$work_dir/upload.log")
client_download_bytes=$(cat "$work_dir/download.bytes")
[ -n "$source_bytes" ] && [ "$source_bytes" = "$upload_bytes" ] || fail
# The source clock starts before curl connects; server upload elapsed excludes that startup.
[ -n "$source_elapsed" ] && [ "$source_elapsed" -ge "$((duration * 1000))" ] || fail
[ -n "$client_download_bytes" ] && [ "$client_download_bytes" = "$download_bytes" ] || fail
upload_elapsed=$(direction_elapsed upload)
download_elapsed=$(direction_elapsed download)
[ -n "$upload_elapsed" ] && [ -n "$download_elapsed" ] || fail
[ "$download_elapsed" -ge "$(((duration - 1) * 1000))" ] || fail
printf 'source result: bytes=%s elapsed_ms=%s\n' "$source_bytes" "$source_elapsed"
printf 'upload result: %s\n' "$(cat "$work_dir/upload.json")"
printf 'final result: %s\n' "$record"
printf '%s_PASSED\n' "$marker"
