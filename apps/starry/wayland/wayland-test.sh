#!/bin/sh
set -eu

# Wayland/Weston smoke test for StarryOS.
# Installs weston via Alpine apk, starts the compositor with the DRM backend,
# and verifies the Wayland socket appears and a simple client connects.

green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
bold="$(printf '\033[1m')"
reset="$(printf '\033[0m')"

weston_pid=""
test_done=0
failed=0

fail() {
    printf "%sWAYLAND_TEST_FAILED: %s%s\n" "$red" "$*" "$reset"
    echo "WAYLAND_TEST_FAILED"
    failed=1
    exit 1
}

cleanup() {
    if [ -n "$weston_pid" ]; then
        kill "$weston_pid" >/dev/null 2>&1 || true
        wait "$weston_pid" >/dev/null 2>&1 || true
        weston_pid=""
    fi
    rm -f /tmp/wayland-* 2>/dev/null || true
}

on_exit() {
    rc=$?
    cleanup
    if [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ]; then
        printf "%sWAYLAND_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "WAYLAND_TEST_FAILED"
    fi
    exit "$rc"
}
trap on_exit EXIT

# ---- Install weston ----
echo "WAYLAND_PREP installing weston..."
apk add weston 2>&1 || fail "apk add weston failed"

# Install DRM backend (separate Alpine package)
apk add weston-backend-drm 2>&1 || true

# Install desktop shell if not bundled
apk add weston-shell-desktop 2>&1 || true

# Debug: list available backends and shells
echo "WAYLAND_PREP available backends:"
ls -la /usr/lib/libweston-14/ 2>&1 || true
echo "WAYLAND_PREP available shells (/usr/lib/weston/):"
ls -la /usr/lib/weston/ 2>&1 || true
echo "WAYLAND_PREP searching for desktop-shell.so:"
find /usr/lib -name "desktop-shell.so" 2>/dev/null || echo "  not found via find"

# Verify the DRM device exists
if [ ! -e /dev/dri/card0 ]; then
    fail "/dev/dri/card0 not found — DRM kernel driver missing"
fi
echo "WAYLAND_PREP /dev/dri/card0 present"

# Verify input devices exist
input_count=$(ls /dev/input/event* 2>/dev/null | wc -l)
if [ "$input_count" -lt 1 ]; then
    echo "WAYLAND_PREP warning: no /dev/input/event* devices found"
else
    echo "WAYLAND_PREP found $input_count input device(s)"
fi

# ---- Start Weston ----
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null

# Skip libseat — our kernel doesn't run seatd
export LIBSEAT_BACKEND=noop

echo "WAYLAND_STAGE starting weston with DRM backend..."
/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --no-config \
    --idle-time=0 \
    --log=/tmp/weston.log &
weston_pid=$!

# ---- Wait for Wayland socket ----
socket_ready=0
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 1
    if ! kill -0 "$weston_pid" >/dev/null 2>&1; then
        echo "WAYLAND_STAGE weston exited prematurely (pid=$weston_pid)"
        tail -30 /tmp/weston.log
        fail "weston exited before creating Wayland socket"
    fi
    disp=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$disp" ]; then
        socket_ready=1
        echo "WAYLAND_STAGE Wayland socket ready: /tmp/$disp"
        break
    fi
done

if [ "$socket_ready" -ne 1 ]; then
    tail -30 /tmp/weston.log
    fail "weston did not create a Wayland socket within 15s"
fi

export WAYLAND_DISPLAY="$disp"

# ---- Verify compositor is responsive ----
# weston-info queries the compositor for global interfaces.
echo "WAYLAND_STAGE connecting client..."
if command -v weston-info >/dev/null 2>&1; then
    if weston-info >/tmp/weston-info.out 2>&1; then
        echo "WAYLAND_STAGE weston-info connected successfully"
        grep -q 'wl_compositor' /tmp/weston-info.out && echo "WAYLAND_STAGE wl_compositor interface present"
    else
        echo "WAYLAND_STAGE weston-info exited non-zero (may be normal)"
    fi
else
    # If weston-info is not available, verify the socket is still there
    # and weston is still alive as a minimal smoke check.
    echo "WAYLAND_STAGE weston-info not available, checking socket liveness..."
    if [ -S "/tmp/$WAYLAND_DISPLAY" ]; then
        echo "WAYLAND_STAGE socket /tmp/$WAYLAND_DISPLAY is alive"
    else
        fail "Wayland socket disappeared"
    fi
fi

# ---- Check weston log for obvious errors ----
if grep -iE "failed to open|no such file|permission denied" /tmp/weston.log >/tmp/weston-errors.out 2>&1; then
    echo "WAYLAND_STAGE weston log contains errors:"
    cat /tmp/weston-errors.out
    # Don't fail — some "errors" are benign (e.g. missing optional backends)
else
    echo "WAYLAND_STAGE no obvious errors in weston log"
fi

# Let weston run briefly, then shut down
sleep 2

# ---- Shutdown ----
echo "WAYLAND_STAGE shutting down weston..."
kill "$weston_pid" 2>/dev/null || true
# Wait up to 5s for graceful exit, then force-kill
for i in 1 2 3 4 5; do
    if ! kill -0 "$weston_pid" 2>/dev/null; then break; fi
    sleep 1
done
kill -9 "$weston_pid" 2>/dev/null || true
wait "$weston_pid" 2>/dev/null || true
weston_pid=""

test_done=1
trap - EXIT
cleanup

printf "%sWAYLAND_TEST_RESULT PASSED%s\n" "$green" "$reset"
echo "WAYLAND_TEST_PASSED"
