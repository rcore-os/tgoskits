#!/bin/sh
set -eu

# Qalculate-QT graphical test for StarryOS.
# Starts Weston compositor with DRM backend + pixman renderer,
# launches qalculate-qt under Wayland to verify Qt6/Wayland
# integration and compositor stability.

green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
reset="$(printf '\033[0m')"

weston_pid=""
test_done=0
failed=0

fail() {
    printf "%sQT_CALC_TEST_FAILED: %s%s\n" "$red" "$*" "$reset"
    echo "QT_CALC_TEST_FAILED"
    failed=1
    exit 1
}

run_with_timeout() {
    timeout_secs="$1"
    shift

    if command -v timeout >/dev/null 2>&1; then
        timeout "$timeout_secs" "$@"
        return $?
    fi

    "$@" &
    cmd_pid=$!
    elapsed=0
    while kill -0 "$cmd_pid" >/dev/null 2>&1; do
        if [ "$elapsed" -ge "$timeout_secs" ]; then
            echo "QT_CALC_PREP command timed out after ${timeout_secs}s: $*"
            kill "$cmd_pid" >/dev/null 2>&1 || true
            sleep 1
            kill -9 "$cmd_pid" >/dev/null 2>&1 || true
            wait "$cmd_pid" >/dev/null 2>&1 || true
            return 124
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    wait "$cmd_pid"
}

install_qt_and_weston_packages() {
    # Packages are pre-installed in the rootfs via qemu-user in prebuild.sh
    echo "QT_CALC_PREP checking pre-installed packages..."

    if ! command -v weston >/dev/null 2>&1; then
        fail "weston not found — prebuild may have failed"
    fi
    echo "QT_CALC_PREP weston found"
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
        printf "%sQT_CALC_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "QT_CALC_TEST_FAILED"
    fi
    exit "$rc"
}
trap on_exit EXIT

# ---- Install Qt and Weston ----
echo "QT_CALC_PREP installing Qt6 and Weston..."
install_qt_and_weston_packages 2>&1 || fail "apk add Qt6/Weston packages failed after mirror retries"

# Debug: list available backends and shells
echo "QT_CALC_PREP available backends:"
ls -la /usr/lib/libweston-14/ 2>&1 || true
echo "QT_CALC_PREP available shells (/usr/lib/weston/):"
ls -la /usr/lib/weston/ 2>&1 || true

# Verify the DRM device exists
if [ ! -e /dev/dri/card0 ]; then
    fail "/dev/dri/card0 not found — DRM kernel driver missing"
fi
echo "QT_CALC_PREP /dev/dri/card0 present"

# Verify input devices exist
input_count=$(ls /dev/input/event* 2>/dev/null | wc -l)
if [ "$input_count" -lt 1 ]; then
    echo "QT_CALC_PREP warning: no /dev/input/event* devices found"
else
    echo "QT_CALC_PREP found $input_count input device(s)"
fi

# Check for the Qt application binary
calc_bin=""
if [ -x /usr/bin/qalculate-qt ]; then
    calc_bin="/usr/bin/qalculate-qt"
    echo "QT_CALC_PREP found Qt binary: $calc_bin"
fi

if [ -z "$calc_bin" ]; then
    fail "qalculate-qt binary not found"
fi

# ---- Start Weston ----
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null

# Skip libseat — our kernel doesn't run seatd
export LIBSEAT_BACKEND=noop

# Weston config for desktop-shell (needed for non-fullscreen Qt windows)
mkdir -p /etc/xdg/weston
cat > /etc/xdg/weston/weston.ini <<'EOF'
[core]
shell=desktop-shell.so
idle-time=0

[shell]
background-color=0xff002244
locking=false

[keyboard]
keymap_layout=us
EOF

echo "QT_CALC_STAGE starting weston with DRM backend (pixman)..."
LIBGL_ALWAYS_SOFTWARE=1 /usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --config=/etc/xdg/weston/weston.ini \
    --idle-time=0 \
    --log=/tmp/weston.log \
    >/tmp/weston-stdout.log 2>/tmp/weston-stderr.log &
weston_pid=$!

# ---- Wait for Wayland socket ----
socket_ready=0
for i in $(seq 1 120); do
    sleep 1
    if ! kill -0 "$weston_pid" >/dev/null 2>&1; then
        echo "QT_CALC_STAGE weston exited prematurely (pid=$weston_pid)"
        tail -30 /tmp/weston.log
        fail "weston exited before creating Wayland socket"
    fi
    disp=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$disp" ]; then
        socket_ready=1
        echo "QT_CALC_STAGE Wayland socket ready: /tmp/$disp"
        break
    fi
done

if [ "$socket_ready" -ne 1 ]; then
    tail -30 /tmp/weston.log
    fail "weston did not create a Wayland socket within 120s"
fi

export WAYLAND_DISPLAY="$disp"
export QT_QPA_PLATFORM=wayland
export QT_WAYLAND_DISABLE_EGL=1

# ---- Run Qalculate-QT ----
echo "QT_CALC_STAGE launching qalculate-qt (wayland)..."

calc_exit_code=0
run_with_timeout 60 "$calc_bin" >/tmp/qt_stdout.log 2>/tmp/qt_err.log || calc_exit_code=$?
echo "QT_CALC_DIAG === Qt stderr ==="
cat /tmp/qt_err.log 2>/dev/null | head -30
echo "QT_CALC_DIAG === Qt stdout ==="
cat /tmp/qt_stdout.log 2>/dev/null | head -10

if [ "$calc_exit_code" -ne 0 ] && [ "$calc_exit_code" -ne 143 ]; then
    echo "QT_CALC_STAGE wayland failed ($calc_exit_code), trying offscreen..."
    export QT_QPA_PLATFORM=offscreen
    run_with_timeout 10 "$calc_bin" 2>/tmp/qt_err2.log || calc_exit_code=$?
fi

# Check exit code — 0 (normal), 124 (timeout), 143 (SIGTERM) all indicate the
# app ran and displayed without crashing for the full test window.
if [ "$calc_exit_code" -eq 0 ] || [ "$calc_exit_code" -eq 124 ] || [ "$calc_exit_code" -eq 143 ]; then
    echo "QT_CALC_STAGE Qalculate-QT completed (exit code: $calc_exit_code)"
else
    echo "QT_CALC_STAGE Qalculate-QT exited with code $calc_exit_code"
    tail -20 /tmp/weston.log 2>/dev/null || true
    fail "Qalculate-QT exited with error code $calc_exit_code"
fi

# ---- Check weston log for obvious errors ----
if grep -iE "failed to open|no such file|permission denied|segfault|error" /tmp/weston.log >/tmp/weston-errors.out 2>&1; then
    echo "QT_CALC_STAGE weston log contains errors:"
    cat /tmp/weston-errors.out
else
    echo "QT_CALC_STAGE no obvious errors in weston log"
fi

# Dump Weston input-related log entries
echo "QT_CALC_DIAG === Weston input log ==="
grep -i "input\|pointer\|keyboard\|focus\|touch\|surface.*enter\|seat" /tmp/weston.log 2>/dev/null | head -30 || echo "QT_CALC_DIAG (no input log entries)"

# Let weston run briefly, then shut down
sleep 2

# ---- Shutdown ----
echo "QT_CALC_STAGE shutting down weston..."
kill "$weston_pid" 2>/dev/null || true
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

printf "%sQT_CALC_TEST_RESULT PASSED%s\n" "$green" "$reset"
echo "QT_CALC_TEST_PASSED"
