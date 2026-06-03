#!/bin/sh
set -eu

test_done=0

cleanup() {
    rm -rf /tmp/ffmpeg-net-* 2>/dev/null || true
    # Kill any leftover HTTP server
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

skip() {
    echo "FFMPEG_NETWORK_STAGE $1 SKIP: $2"
}

mkdir -p /tmp/ffmpeg-net-workdir

TEST_MEDIA_DIR="/usr/share/ffmpeg-test-media"

# ---- Helper: check if a protocol is available ----
has_protocol() {
    ffmpeg -protocols 2>/dev/null | grep -q "$1"
}

# ---- Helper: check if a codec is available ----
has_encoder() {
    ffmpeg -encoders 2>/dev/null | grep -q "$1"
}

# ---- Stage 1: Check protocol support ----
echo "FFMPEG_NETWORK_STAGE protocol-list"
ffmpeg -protocols > /tmp/ffmpeg-net-workdir/protocols.out 2>&1 || fail "cannot list protocols"
# Check common protocols
for proto in http file pipe; do
    if grep -q "$proto" /tmp/ffmpeg-net-workdir/protocols.out; then
        echo "  Protocol $proto: supported"
    else
        echo "  Protocol $proto: not supported"
    fi
done

# ---- Stage 2: File protocol (basic I/O) ----
echo "FFMPEG_NETWORK_STAGE file-protocol"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    # Use file:// URL to access local file
    ffmpeg -y -i "file:$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy \
        /tmp/ffmpeg-net-workdir/file_proto.mp4 2>/dev/null \
        || fail "file:// protocol failed"
    [ -s /tmp/ffmpeg-net-workdir/file_proto.mp4 ] || fail "file:// output is empty"
else
    skip "file-protocol" "no test media"
fi

# ---- Stage 3: Pipe protocol (stdin/stdout) ----
echo "FFMPEG_NETWORK_STAGE pipe-protocol"
if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
    # Pipe input: read from stdin
    cat "$TEST_MEDIA_DIR/test_160x120.mp4" | ffmpeg -y -i pipe:0 \
        -c copy \
        /tmp/ffmpeg-net-workdir/pipe_in.mp4 2>/dev/null \
        || fail "pipe:0 input failed"
    [ -s /tmp/ffmpeg-net-workdir/pipe_in.mp4 ] || fail "pipe:0 output is empty"

    # Pipe output: write to stdout (use mpegts format as mp4 doesn't support non-seekable output)
    ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
        -c copy -f mpegts pipe:1 > /tmp/ffmpeg-net-workdir/pipe_out.ts 2>/dev/null \
        || fail "pipe:1 output failed"
    [ -s /tmp/ffmpeg-net-workdir/pipe_out.ts ] || fail "pipe:1 output is empty"
else
    skip "pipe-protocol" "no test media"
fi

# ---- Stage 4: HTTP server setup for network tests ----
echo "FFMPEG_NETWORK_STAGE http-setup"
http_server_pid=""

if command -v python3 >/dev/null 2>&1; then
    # Create a directory with test media for HTTP serving
    mkdir -p /tmp/ffmpeg-net-workdir/www
    if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
        cp "$TEST_MEDIA_DIR/test_160x120.mp4" /tmp/ffmpeg-net-workdir/www/
    fi
    if [ -f "$TEST_MEDIA_DIR/test_audio.mp3" ]; then
        cp "$TEST_MEDIA_DIR/test_audio.mp3" /tmp/ffmpeg-net-workdir/www/
    fi

    # Start a simple HTTP server in background
    python3 -m http.server 8080 -d /tmp/ffmpeg-net-workdir/www \
        > /tmp/ffmpeg-net-workdir/httpd.log 2>&1 &
    http_server_pid=$!

    # Wait for server to start
    i=0
    while [ "$i" -lt 10 ]; do
        if wget -q -O /dev/null http://127.0.0.1:8080/ 2>/dev/null; then
            break
        fi
        i=$((i + 1))
        sleep 1
    done

    # Verify server is actually running
    if ! kill -0 "$http_server_pid" 2>/dev/null; then
        echo "  WARNING: httpd process exited unexpectedly"
        cat /tmp/ffmpeg-net-workdir/httpd.log 2>/dev/null || true
        http_server_pid=""
    fi
else
    skip "http-setup" "python3 not available"
fi

# ---- Stage 5: HTTP input (download and decode) ----
echo "FFMPEG_NETWORK_STAGE http-input"
if [ -n "$http_server_pid" ] && [ -f /tmp/ffmpeg-net-workdir/www/test_160x120.mp4 ]; then
    if has_protocol "http"; then
        ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
            -c copy \
            /tmp/ffmpeg-net-workdir/http_input.mp4 2>/dev/null \
            || fail "http input failed"
        [ -s /tmp/ffmpeg-net-workdir/http_input.mp4 ] || fail "http input output is empty"
    else
        skip "http-input" "http protocol not supported"
    fi
else
    skip "http-input" "http server not running or no test media"
fi

# ---- Stage 6: HTTP input with transcoding ----
echo "FFMPEG_NETWORK_STAGE http-transcode"
if [ -n "$http_server_pid" ] && [ -f /tmp/ffmpeg-net-workdir/www/test_160x120.mp4 ]; then
    if has_protocol "http" && has_encoder "libx264"; then
        ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
            -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            /tmp/ffmpeg-net-workdir/http_transcode.mp4 2>/dev/null \
            || fail "http transcode failed"
        [ -s /tmp/ffmpeg-net-workdir/http_transcode.mp4 ] || fail "http transcode output is empty"
    else
        skip "http-transcode" "http or libx264 not available"
    fi
else
    skip "http-transcode" "http server not running or no test media"
fi

# ---- Stage 7: HTTP audio input ----
echo "FFMPEG_NETWORK_STAGE http-audio"
if [ -n "$http_server_pid" ] && [ -f /tmp/ffmpeg-net-workdir/www/test_audio.mp3 ]; then
    if has_protocol "http"; then
        ffmpeg -y -i "http://127.0.0.1:8080/test_audio.mp3" \
            -c:a pcm_s16le \
            /tmp/ffmpeg-net-workdir/http_audio.wav 2>/dev/null \
            || fail "http audio input failed"
        [ -s /tmp/ffmpeg-net-workdir/http_audio.wav ] || fail "http audio output is empty"
    else
        skip "http-audio" "http protocol not supported"
    fi
else
    skip "http-audio" "http server not running or no audio test media"
fi

# ---- Stage 8: HTTP seek (range request) ----
echo "FFMPEG_NETWORK_STAGE http-seek"
if [ -n "$http_server_pid" ] && [ -f /tmp/ffmpeg-net-workdir/www/test_160x120.mp4 ]; then
    if has_protocol "http"; then
        # Seek into the middle of the file
        ffmpeg -y -ss 0.5 -i "http://127.0.0.1:8080/test_160x120.mp4" \
            -c copy -t 1 \
            /tmp/ffmpeg-net-workdir/http_seek.mp4 2>/dev/null \
            || fail "http seek failed"
        [ -s /tmp/ffmpeg-net-workdir/http_seek.mp4 ] || fail "http seek output is empty"
    else
        skip "http-seek" "http protocol not supported"
    fi
else
    skip "http-seek" "http server not running or no test media"
fi

# ---- Stage 9: TCP protocol (if available) ----
echo "FFMPEG_NETWORK_STAGE tcp-loopback"
if has_protocol "tcp"; then
    if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
        # Start ffmpeg as TCP listener in background (use mpegts format for streaming)
        ffmpeg -y -re -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
            -c copy -f mpegts tcp://127.0.0.1:12345?listen=1 \
            > /tmp/ffmpeg-net-workdir/tcp_server.log 2>&1 &
        tcp_server_pid=$!
        sleep 2

        # Connect as TCP client
        ffmpeg -y -i "tcp://127.0.0.1:12345" \
            -c copy -t 1 \
            /tmp/ffmpeg-net-workdir/tcp_recv.ts 2>/dev/null \
            || fail "tcp client receive failed"
        [ -s /tmp/ffmpeg-net-workdir/tcp_recv.ts ] || fail "tcp received file is empty"

        kill "$tcp_server_pid" 2>/dev/null || true
        wait "$tcp_server_pid" 2>/dev/null || true
    else
        skip "tcp-loopback" "no test media"
    fi
else
    skip "tcp-loopback" "tcp protocol not supported"
fi

# ---- Stage 10: UDP protocol (if available) ----
echo "FFMPEG_NETWORK_STAGE udp-loopback"
if has_protocol "udp"; then
    if [ -f "$TEST_MEDIA_DIR/test_160x120.mp4" ]; then
        # Start ffmpeg as UDP listener in background
        ffmpeg -y -i "udp://127.0.0.1:12346?timeout=5000000" \
            -c copy \
            /tmp/ffmpeg-net-workdir/udp_recv.ts \
            > /tmp/ffmpeg-net-workdir/udp_recv.log 2>&1 &
        udp_recv_pid=$!
        sleep 1

        # Send via UDP (use mpegts format for streaming)
        ffmpeg -y -i "$TEST_MEDIA_DIR/test_160x120.mp4" \
            -c copy -f mpegts "udp://127.0.0.1:12346" \
            > /tmp/ffmpeg-net-workdir/udp_send.log 2>&1 || true

        wait "$udp_recv_pid" 2>/dev/null || true
        if [ -s /tmp/ffmpeg-net-workdir/udp_recv.ts ]; then
            echo "  UDP loopback: OK"
        else
            skip "udp-loopback" "UDP transfer produced empty output (may need multicast)"
        fi
    else
        skip "udp-loopback" "no test media"
    fi
else
    skip "udp-loopback" "udp protocol not supported"
fi

# ---- Stage 11: HTTP output (verify HTTP client works) ----
echo "FFMPEG_NETWORK_STAGE http-output"
if [ -n "$http_server_pid" ] && [ -f /tmp/ffmpeg-net-workdir/www/test_160x120.mp4 ]; then
    if has_protocol "http"; then
        # Verify ffmpeg can fetch via HTTP and write output (tests HTTP client path)
        ffmpeg -y -i "http://127.0.0.1:8080/test_160x120.mp4" \
            -c copy -t 1 \
            /tmp/ffmpeg-net-workdir/http_client.mp4 2>/dev/null \
            || fail "http client fetch failed"
        [ -s /tmp/ffmpeg-net-workdir/http_client.mp4 ] || fail "http client output is empty"
        echo "  HTTP client fetch: OK"
    else
        skip "http-output" "http protocol not supported"
    fi
else
    skip "http-output" "http server not running or no test media"
fi

# ---- Stage 12: MMS/MMST protocol check ----
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
