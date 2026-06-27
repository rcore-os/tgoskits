#!/bin/sh
set +e

export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export XDG_RUNTIME_DIR=/tmp
export LIBSEAT_BACKEND=noop
export DISPLAY=
export TERM=dumb
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null

PASS=0
FAIL=0

pass() { PASS=$((PASS+1)); printf "\033[32m[PASS]\033[0m %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); printf "\033[31m[FAIL]\033[0m %s\n" "$*"; }
info() { printf "\033[33m[INFO]\033[0m %s\n" "$*"; }
hdr()  { printf "\n\033[1;33m=== %s ===\033[0m\n" "$*"; }

printf '\033[?1000l\033[?1002l\033[?1003l\033[?1006l' 2>/dev/null || true

hdr "L1: Weston compositor (pixman renderer, 214x120)"
[ ! -e /dev/dri/card0 ] && { fail "L1: no DRM device"; exit 1; }

rm -f /tmp/weston.log /tmp/weston-stderr.log
/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --shell=kiosk-shell.so \
    --no-config \
    --idle-time=0 \
    --log=/tmp/weston.log \
    >/tmp/weston-stdout.log 2>/tmp/weston-stderr.log &
WESTON_PID=$!

# pixman 渲染的 Weston 启动很快，但给 60s 超时留余量
WAYLAND_TIMEOUT=60
READY=0
for i in $(seq 1 $WAYLAND_TIMEOUT); do
    sleep 1
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$DISP" ]; then READY=1; break; fi
    if ! kill -0 "$WESTON_PID" 2>/dev/null; then
        fail "L1: Weston exited during init (after ${i}s)"
        echo "=== Weston log (last 30 lines) ==="
        tail -30 /tmp/weston.log 2>&1 || true
        echo "=== Weston stderr ==="
        cat /tmp/weston-stderr.log 2>&1 || true
        echo "DOOMGENERIC_TEST_FAILED"; exit 1
    fi
    [ $((i % 10)) -eq 0 ] && info "L1: waiting for Wayland socket... ${i}s"
done
[ "$READY" -eq 1 ] && pass "L1: Wayland socket /tmp/$DISP" \
    || { fail "L1: no Wayland socket after ${WAYLAND_TIMEOUT}s"; cat /tmp/weston.log 2>&1; echo "DOOMGENERIC_TEST_FAILED"; exit 1; }

export WAYLAND_DISPLAY="$DISP"

hdr "L2: doomgeneric SDL2 Wayland (214x120)"
IWAD="/usr/share/games/doom/freedoom2.wad"
if [ ! -f "$IWAD" ]; then
    IWAD="/usr/share/games/doom/freedoom1.wad"
fi

if [ -f "$IWAD" ]; then
    echo "--- starting doomgeneric (SDL2 Wayland backend) ---"
    rm -f /tmp/doomgeneric_stdout.log /tmp/doomgeneric_stderr.log

    export DOOMWADPATH=/usr/share/games/doom
    SDL_RENDER_DRIVER=opengles2 \
    SDL_VIDEODRIVER=wayland \
    SDL_AUDIODRIVER=dummy \
    /usr/bin/doomgeneric \
        -iwad "$IWAD" \
        -nosound \
        >/tmp/doomgeneric_stdout.log 2>/tmp/doomgeneric_stderr.log &
    DOOM_PID=$!

    sleep 5

    if kill -0 "$DOOM_PID" 2>/dev/null; then
        pass "L2: doomgeneric process alive (pid=$DOOM_PID)"
    else
        wait "$DOOM_PID" 2>/dev/null
        RC=$?
        case $RC in
            139) fail "L2: doomgeneric SIGSEGV" ;;
            *)   fail "L2: doomgeneric exited early (exit=$RC)" ;;
        esac
    fi

    sleep 30

    if kill -0 "$DOOM_PID" 2>/dev/null; then
        pass "L2: doomgeneric survived 35s (pid=$DOOM_PID)"
        kill "$DOOM_PID" 2>/dev/null || true
    else
        wait "$DOOM_PID" 2>/dev/null
        RC=$?
        case $RC in
            139) fail "L2: doomgeneric SIGSEGV" ;;
            *)   fail "L2: doomgeneric exited early (exit=$RC)" ;;
        esac
    fi
else
    fail "L2: no freedoom IWAD found"
    ls /usr/share/games/doom/ 2>&1
fi

hdr "L3: Diagnostic dump"
echo "--- weston log (last 50 lines) ---"
tail -50 /tmp/weston.log 2>&1 || true
echo "--- weston stderr ---"
cat /tmp/weston-stderr.log 2>&1 || true
echo "--- doomgeneric stderr (raw) ---"
cat /tmp/doomgeneric_stderr.log 2>&1
echo "--- doomgeneric stdout (raw) ---"
cat /tmp/doomgeneric_stdout.log 2>&1
echo "--- SDL/renderer info ---"
grep -iE 'renderer|error|failed' /tmp/doomgeneric_stderr.log 2>/dev/null || echo "(no renderer info found)"

hdr "Cleanup"
kill "$DOOM_PID" 2>/dev/null || true
kill "$WESTON_PID" 2>/dev/null || true
for i in $(seq 1 5); do
    kill -0 "$WESTON_PID" 2>/dev/null || break
    sleep 1
done
kill -9 "$WESTON_PID" 2>/dev/null || true
wait "$WESTON_PID" 2>/dev/null || true

hdr "SUMMARY"
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ] && echo "DOOMGENERIC_TEST_PASSED" || echo "DOOMGENERIC_TEST_FAILED"
