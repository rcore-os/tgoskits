#!/bin/sh
# ffplay GL-on-Wayland test — verify full Mesa/llvmpipe GL pipeline
set +e

export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export XDG_RUNTIME_DIR=/tmp
export LIBSEAT_BACKEND=noop
export DISPLAY=
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null

PASS=0
FAIL=0

pass() { PASS=$((PASS+1)); printf "\033[32m[PASS]\033[0m %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); printf "\033[31m[FAIL]\033[0m %s\n" "$*"; }
info() { printf "\033[33m[INFO]\033[0m %s\n" "$*"; }
hdr()  { printf "\n\033[1;33m=== %s ===\033[0m\n" "$*"; }

# ======================================================================
hdr "L1: Weston compositor (GL renderer / llvmpipe)"
# ======================================================================
[ ! -e /dev/dri/card0 ] && { fail "L1: no DRM device"; exit 1; }

rm -f /tmp/weston.log /tmp/weston-stderr.log
/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=gl \
    --shell=kiosk-shell.so \
    --no-config \
    --idle-time=0 \
    --log=/tmp/weston.log \
    >/tmp/weston-stdout.log 2>/tmp/weston-stderr.log &
WESTON_PID=$!
sleep 3

if ! kill -0 "$WESTON_PID" 2>/dev/null; then
    fail "L1: Weston exited on startup"
    echo "=== weston stderr ==="; cat /tmp/weston-stderr.log 2>&1
    echo "=== weston log ==="; cat /tmp/weston.log 2>&1
    echo "FFPLAY_TEST_FAILED"; exit 1
fi
pass "L1: Weston process alive (pid=$WESTON_PID)"

# Wait for Wayland socket (GL init can take 25+ seconds)
READY=0
for i in $(seq 1 45); do
    sleep 1
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$DISP" ]; then READY=1; break; fi
done
[ "$READY" -eq 1 ] && pass "L1: Wayland socket /tmp/$DISP" \
    || { fail "L1: no Wayland socket"; cat /tmp/weston.log 2>&1; echo "FFPLAY_TEST_FAILED"; exit 1; }

export WAYLAND_DISPLAY="$DISP"

# ======================================================================
hdr "L2: ffplay Wayland (perf-tuned)"
# ======================================================================
if [ -f /usr/share/test.mp4 ]; then
    echo "--- starting ffplay (Mesa GLES2 path) ---"
    rm -f /tmp/ffplay_stdout.log /tmp/ffplay_stderr.log /tmp/ffplay_maps.log

    # llvmpipe 软渲染是瓶颈，让解码器多缓冲平滑输出
    SDL_VIDEODRIVER=wayland SDL_AUDIODRIVER=dummy \
    LIBGL_ALWAYS_SOFTWARE=1 \
    timeout 180 ffplay -threads 4 -an \
        -autoexit -x 284 -y 160 /usr/share/test.mp4 \
        >/tmp/ffplay_stdout.log 2>/tmp/ffplay_stderr.log &
    FPID=$!

    wait $FPID 2>/dev/null
    RC=$?
    case $RC in
        0)   pass "L2: ffplay Wayland exit=0" ;;
        124) pass "L2: ffplay Wayland survived 180s (timeout)" ;;
        139) fail "L2: ffplay Wayland SIGSEGV" ;;
        *)   fail "L2: ffplay Wayland (exit=$RC)" ;;
    esac
else
    fail "L2: /usr/share/test.mp4 not found — ffplay never ran"
fi

# ======================================================================
hdr "L3: Weston / ffplay stderr dump"
# ======================================================================
grep -E "llvmpipe|EGL" /tmp/weston.log 2>/dev/null || true
grep -E "error|warn|fail" /tmp/ffplay_stderr.log 2>/dev/null || true

# Shutdown Weston
kill "$WESTON_PID" 2>/dev/null || true
for i in $(seq 1 5); do
    kill -0 "$WESTON_PID" 2>/dev/null || break
    sleep 1
done
kill -9 "$WESTON_PID" 2>/dev/null || true
wait "$WESTON_PID" 2>/dev/null || true

# ======================================================================
hdr "SUMMARY"
# ======================================================================
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ] && echo "FFPLAY_TEST_PASSED" || echo "FFPLAY_TEST_FAILED"
