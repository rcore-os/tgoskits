#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -rf /tmp/ffmpeg-net-* 2>/dev/null || true
    if [ -n "${http_server_pid:-}" ]; then
        kill "$http_server_pid" 2>/dev/null || true
        wait "$http_server_pid" 2>/dev/null || true
    fi
}

on_exit() {
    rc=$?
    if [ "$test_done" -ne 1 ]; then
        echo "FFMPEG_NETWORK_TEST_FAILED"
    fi
    cleanup
    exit "$rc"
}
trap on_exit EXIT

fail() {
    echo "FFMPEG_NETWORK_STAGE FAILED: $1"
    echo "FFMPEG_NETWORK_TEST_FAILED"
    exit 1
}

mkdir -p /tmp/ffmpeg-net-workdir

# ---- Generate test media if not pre-built ----
. /usr/bin/ffmpeg-ensure-media.sh
TEST_MEDIA_DIR="$FFMPEG_TEST_MEDIA_DIR"

has_protocol() {
    ffmpeg -protocols 2>/dev/null | grep -q "$1"
}

has_encoder() {
    ffmpeg -encoders 2>/dev/null | grep -q "$1"
}

# ---- Protocol support check ----
echo "FFMPEG_NETWORK_STAGE protocol-list"
ffmpeg -protocols > /tmp/ffmpeg-net-workdir/protocols.out 2>&1 || fail "cannot list protocols"
for proto in http file pipe; do
    if grep -q "$proto" /tmp/ffmpeg-net-workdir/protocols.out; then
        echo "  Protocol $proto: supported"
    else
        echo "  Protocol $proto: not supported"
    fi
done

# ---- File protocol ----
echo "FFMPEG_NETWORK_STAGE file-protocol"
ffmpeg -y -i "file:$TEST_MEDIA_DIR/test_160x120.mp4" \
    -c copy \
    /tmp/ffmpeg-net-workdir/file_proto.mp4 2>/dev/null \
    || fail "file:// protocol failed"
[ -s /tmp/ffmpeg-net-workdir/file_proto.mp4 ] || fail "file:// output is empty"

# ---- Pipe protocol ----
echo "FFMPEG_NETWORK_STAGE pipe-protocol"
cat "$TEST_MEDIA_DIR/test_160x120.mp4" | ffmpeg -y -i pipe:0 \
    -c copy \
    /tmp/ffmpeg-net-workdir/pipe_in.mp4 2>/dev/null \
    || fail "pipe:0 input failed"
[ -s /tmp/ffmpeg-net-workdir/pipe_in.mp4 ] || fail "pipe:0 output is empty"

ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -c copy -f mpegts pipe:1 > /tmp/ffmpeg-net-workdir/pipe_out.ts 2>/dev/null \
    || fail "pipe:1 output failed"
[ -s /tmp/ffmpeg-net-workdir/pipe_out.ts ] || fail "pipe:1 output is empty"

# ---- HTTP server setup ----
echo "FFMPEG_NETWORK_STAGE http-setup"
http_server_pid=""

command -v python3 >/dev/null 2>&1 || fail "python3 not available for HTTP tests"

mkdir -p /tmp/ffmpeg-net-workdir/www
cp "$TEST_MEDIA_DIR/test_160x120.mp4" /tmp/ffmpeg-net-workdir/www/
cp "$TEST_MEDIA_DIR/test_audio.mp3" /tmp/ffmpeg-net-workdir/www/

python3 -m http.server 8080 -d /tmp/ffmpeg-net-workdir/www \
    > /tmp/ffmpeg-net-workdir/httpd.log 2>&1 &
http_server_pid=$!

i=0
while [ "$i" -lt 10 ]; do
    if wget -q -O /dev/null http://127.0.0.1:8080/ 2>/dev/null; then
        break
    fi
    i=$((i + 1))
    sleep 1
done

kill -0 "$http_server_pid" 2>/dev/null || fail "httpd process exited unexpectedly"

# ---- HTTP input ----
echo "FFMPEG_NETWORK_STAGE http-input"
has_protocol "http" || fail "http protocol not supported"
ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
    -c copy \
    /tmp/ffmpeg-net-workdir/http_input.mp4 2>/dev/null \
    || fail "http input failed"
[ -s /tmp/ffmpeg-net-workdir/http_input.mp4 ] || fail "http input output is empty"

# ---- HTTP transcode ----
echo "FFMPEG_NETWORK_STAGE http-transcode"
has_encoder "libx264" || fail "encoder libx264 not available for http transcode"
ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    /tmp/ffmpeg-net-workdir/http_transcode.mp4 2>/dev/null \
    || fail "http transcode failed"
[ -s /tmp/ffmpeg-net-workdir/http_transcode.mp4 ] || fail "http transcode output is empty"

# ---- HTTP audio input ----
echo "FFMPEG_NETWORK_STAGE http-audio"
ffmpeg -y -i "http://127.0.0.1:8080/test_audio.mp3" \
    -c:a pcm_s16le \
    /tmp/ffmpeg-net-workdir/http_audio.wav 2>/dev/null \
    || fail "http audio input failed"
[ -s /tmp/ffmpeg-net-workdir/http_audio.wav ] || fail "http audio output is empty"

# ---- HTTP seek ----
echo "FFMPEG_NETWORK_STAGE http-seek"
ffmpeg -y -ss 0.5 -i "http://127.0.0.1:8080/test_160x120.mp4" \
    -c copy -t 1 \
    /tmp/ffmpeg-net-workdir/http_seek.mp4 2>/dev/null \
    || fail "http seek failed"
[ -s /tmp/ffmpeg-net-workdir/http_seek.mp4 ] || fail "http seek output is empty"

# ---- TCP loopback ----
echo "FFMPEG_NETWORK_STAGE tcp-loopback"
has_protocol "tcp" || fail "tcp protocol not supported"
ffmpeg -y -re -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
    -c copy -f mpegts tcp://127.0.0.1:12345?listen=1 \
    > /tmp/ffmpeg-net-workdir/tcp_server.log 2>&1 &
tcp_server_pid=$!
sleep 2

ffmpeg -y -i "tcp://127.0.0.1:12345" \
    -c copy -t 1 \
    /tmp/ffmpeg-net-workdir/tcp_recv.ts 2>/dev/null \
    || fail "tcp client receive failed"
[ -s /tmp/ffmpeg-net-workdir/tcp_recv.ts ] || fail "tcp received file is empty"

kill "$tcp_server_pid" 2>/dev/null || true
wait "$tcp_server_pid" 2>/dev/null || true

# ---- UDP loopback ----
echo "FFMPEG_NETWORK_STAGE udp-loopback"
has_protocol "udp" || fail "udp protocol not supported"
udp_ok=0
udp_attempt=0
while [ "$udp_attempt" -lt 3 ]; do
    udp_port=$((12346 + udp_attempt))
    rm -f /tmp/ffmpeg-net-workdir/udp_recv.ts

    ffmpeg -y -fflags +genpts -i "udp://127.0.0.1:${udp_port}?timeout=10000000&overrun_nonfatal=1&fifo_size=1000000" \
        -c copy -t 2 \
        /tmp/ffmpeg-net-workdir/udp_recv.ts \
        > /tmp/ffmpeg-net-workdir/udp_recv.log 2>&1 &
    udp_recv_pid=$!
    sleep 2

    ffmpeg -y -re -stream_loop 8 -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy -f mpegts "udp://127.0.0.1:${udp_port}?pkt_size=1316" \
        > /tmp/ffmpeg-net-workdir/udp_send.log 2>&1 &
    udp_send_pid=$!

    wait "$udp_recv_pid" 2>/dev/null || true
    kill "$udp_send_pid" 2>/dev/null || true
    wait "$udp_send_pid" 2>/dev/null || true

    if [ -s /tmp/ffmpeg-net-workdir/udp_recv.ts ]; then
        udp_ok=1
        break
    fi

    echo "  UDP loopback attempt $((udp_attempt + 1)) produced empty output"
    udp_attempt=$((udp_attempt + 1))
    sleep 1
done
[ "$udp_ok" -eq 1 ] || fail "UDP transfer produced empty output"

# ---- HTTP output (client fetch) ----
echo "FFMPEG_NETWORK_STAGE http-output"
ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
    -c copy -t 1 \
    /tmp/ffmpeg-net-workdir/http_client.mp4 2>/dev/null \
    || fail "http client fetch failed"
[ -s /tmp/ffmpeg-net-workdir/http_client.mp4 ] || fail "http client output is empty"
echo "  HTTP client fetch: OK"

# ---- Extended protocol check ----
echo "FFMPEG_NETWORK_STAGE protocol-check-extended"
for proto in rtmp rtsp mms mmst rtp srt; do
    if has_protocol "$proto"; then
        echo "  Protocol $proto: supported"
    else
        echo "  Protocol $proto: not supported"
    fi
done

# Cleanup HTTP server
if [ -n "$http_server_pid" ]; then
    kill "$http_server_pid" 2>/dev/null || true
    wait "$http_server_pid" 2>/dev/null || true
    http_server_pid=""
fi

test_done=1
trap - EXIT
cleanup

echo "FFMPEG_NETWORK_TEST_PASSED"
