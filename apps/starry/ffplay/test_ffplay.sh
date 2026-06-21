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
# LIBGL_DEBUG=verbose — Mesa prints EGL/GL initialization progress to stderr,
# captured in /tmp/weston-stderr.log.  Essential for diagnosing slow llvmpipe
# init (LLVM JIT) that can take 60-120s under QEMU TCG.
# LIBGL_ALWAYS_SOFTWARE=1 — skip hardware GPU detection, force llvmpipe
# directly.  Avoids Mesa probing /dev/dri/renderD128 (which we don't have)
# and saves several seconds on slow CPUs.
LIBGL_DEBUG=verbose LIBGL_ALWAYS_SOFTWARE=1 /usr/bin/weston \
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

# Wait for Wayland socket (GL init can take 25+ seconds locally,
# 60-100+ seconds under QEMU TCG without KVM)
WAYLAND_TIMEOUT="${WAYLAND_TIMEOUT:-300}"
READY=0
for i in $(seq 1 "$WAYLAND_TIMEOUT"); do
    sleep 1
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$DISP" ]; then READY=1; break; fi
    # If Weston died, fail early instead of waiting the full timeout
    if ! kill -0 "$WESTON_PID" 2>/dev/null; then
        fail "L1: Weston exited during init (after ${i}s)"
        echo "=== Weston log (last 30 lines) ==="
        tail -30 /tmp/weston.log 2>&1 || echo "(no log)"
        echo "=== Weston stderr ==="
        cat /tmp/weston-stderr.log 2>&1 || echo "(no stderr)"
        echo "FFPLAY_TEST_FAILED"; exit 1
    fi
    # 每 10 秒打印一次进度，方便调试
    [ $((i % 10)) -eq 0 ] && info "L1: waiting for Wayland socket... ${i}s"
done
if [ "$READY" -eq 1 ]; then
    pass "L1: Wayland socket /tmp/$DISP"
else
    fail "L1: no Wayland socket after ${WAYLAND_TIMEOUT}s"
    echo "=== Weston log (last 30 lines) ==="
    tail -30 /tmp/weston.log 2>&1 || echo "(no log)"
    echo "=== Weston stderr ==="
    cat /tmp/weston-stderr.log 2>&1 || echo "(no stderr)"
    echo "FFPLAY_TEST_FAILED"; exit 1
fi

export WAYLAND_DISPLAY="$DISP"

# ======================================================================
hdr "L2: ffplay Wayland (perf-tuned)"
# ======================================================================
if [ -f /usr/share/test.mp4 ]; then
    echo "--- starting ffplay (Mesa GLES2 path) ---"
    rm -f /tmp/ffplay_stdout.log /tmp/ffplay_stderr.log /tmp/ffplay_maps.log

    # GL 渲染器下需要强制软件渲染，LD_BIND_NOW=1 修复 musl + SDL2 退出时 PLT 解析失败 (exit 123)
    SDL_VIDEODRIVER=wayland SDL_AUDIODRIVER=dummy \
    LIBGL_ALWAYS_SOFTWARE=1 \
    LD_BIND_NOW=1 \
    timeout 300 ffplay -threads 4 -an -loop 0 \
        -x 284 -y 160 /usr/share/test.mp4 \
        >/tmp/ffplay_stdout.log 2>/tmp/ffplay_stderr.log &
    FPID=$!

    wait $FPID 2>/dev/null
    RC=$?
    case $RC in
        0)   pass "L2: ffplay Wayland exit=0" ;;
        123) pass "L2: ffplay Wayland exit=123 (known musl PLT cleanup)" ;;
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
echo "--- Weston log (last 30 lines) ---"
tail -30 /tmp/weston.log 2>/dev/null || echo "(no log)"
echo "--- Weston stderr ---"
cat /tmp/weston-stderr.log 2>/dev/null || echo "(no stderr)"
echo "--- ffplay stderr ---"
cat /tmp/ffplay_stderr.log 2>/dev/null || echo "(no ffplay stderr)"

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
