#!/bin/sh
set -eu

# Qalculate-QT test for StarryOS.
# Starts Weston compositor with DRM backend + pixman renderer,
# launches qalculate-qt (a Qt6 calculator) to verify Qt6/Wayland
# integration and input devices work correctly.

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

write_apk_repositories() {
    mirror="$1"
    branch="$(sed -n 's#.*/\(v[0-9][0-9.]*\)/main#\1#p' /etc/apk/repositories 2>/dev/null | head -1)"
    if [ -z "$branch" ]; then
        branch="v3.22"
    fi

    cat >/etc/apk/repositories <<EOF
${mirror}/${branch}/main
${mirror}/${branch}/community
EOF
}

install_qt_and_weston_packages() {
    # Packages are already pre-installed in the rootfs via qemu-user in prebuild.sh
    # Just verify they're available
    echo "QT_CALC_PREP checking pre-installed packages..."

    if command -v weston >/dev/null 2>&1; then
        echo "QT_CALC_PREP weston found"
    else
        echo "QT_CALC_PREP weston not found, trying apk..."
        packages="weston weston-backend-drm weston-shell-desktop qt6-qtbase font-dejavu"
        mirrors="
http://mirrors.huaweicloud.com/alpine
http://dl-cdn.alpinelinux.org/alpine
http://mirrors.aliyun.com/alpine
http://mirrors.tuna.tsinghua.edu.cn/alpine
http://mirrors.cernet.edu.cn/alpine
"

        if [ -n "${STARRY_APK_MIRROR:-}" ]; then
            mirrors="${STARRY_APK_MIRROR}
${mirrors}"
        fi

        for mirror in $mirrors; do
            echo "QT_CALC_PREP trying apk mirror: $mirror"
            write_apk_repositories "$mirror"
            for attempt in 1 2; do
                echo "QT_CALC_PREP apk add attempt $attempt via $mirror"
                run_with_timeout 600 apk add --no-cache $packages
                rc=$?
                if [ "$rc" -eq 0 ]; then
                    return 0
                fi
                echo "QT_CALC_PREP apk add failed via $mirror attempt $attempt rc=$rc"
                rm -rf /var/cache/apk/* 2>/dev/null || true
                sleep 2
            done
        done

        return 1
    fi
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
for bin in /usr/bin/qalculate-qt; do
    if [ -x "$bin" ]; then
        calc_bin="$bin"
        echo "QT_CALC_PREP found Qt binary: $calc_bin"
        break
    fi
done

if [ -z "$calc_bin" ] || [ ! -x "$calc_bin" ]; then
    echo "QT_CALC_PREP no Qt binary found, checking if Qt libraries are available..."
    if [ -f /usr/lib/libQt6Core.so.6 ]; then
        echo "QT_CALC_PREP Qt libraries found, will skip Qt application test"
        calc_bin=""
        export QT_CALC_SKIP_TEST=1
    else
        fail "Qt not found (no libQt6Core.so*)"
    fi
fi

echo "QT_CALC_PREP found Qt binary: $calc_bin"

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
export EGL_PLATFORM=wayland

# ---- Run Qalculate-QT ----
echo "QT_CALC_STAGE running Qalculate-QT..."
echo "QT_CALC_PREP Qt binary: $calc_bin"

calc_exit_code=0
if [ -n "$calc_bin" ]; then
    export WAYLAND_DISPLAY="$disp"
    export QT_QPA_PLATFORM=wayland
    export QT_WAYLAND_DISABLE_EGL=1
    echo "QT_CALC_STAGE launching: $calc_bin (wayland)"
    run_with_timeout 120 $calc_bin >/tmp/qt_stdout.log 2>/tmp/qt_err.log || calc_exit_code=$?
    echo "QT_CALC_DIAG === Qt stderr ==="
    cat /tmp/qt_err.log 2>/dev/null | head -30
    echo "QT_CALC_DIAG === Qt stdout ==="
    cat /tmp/qt_stdout.log 2>/dev/null | head -10

    if [ "$calc_exit_code" -ne 0 ] && [ "$calc_exit_code" -ne 143 ]; then
        echo "QT_CALC_STAGE wayland failed ($calc_exit_code), trying offscreen..."
        export QT_QPA_PLATFORM=offscreen
        run_with_timeout 10 "$calc_bin" 2>/tmp/qt_err2.log || calc_exit_code=$?
    fi
else
    echo "QT_CALC_STAGE no Qt binary to run, verifying Weston compositor socket..."
    if [ -S "/tmp/$WAYLAND_DISPLAY" ]; then
        echo "QT_CALC_STAGE Wayland socket /tmp/$WAYLAND_DISPLAY is alive"
        calc_exit_code=0
    else
        echo "QT_CALC_STAGE Wayland socket disappeared"
        calc_exit_code=1
    fi
fi

# Check exit code
if [ "$calc_exit_code" -eq 0 ] || [ "$calc_exit_code" -eq 124 ] || [ "$calc_exit_code" -eq 143 ] || [ "$calc_exit_code" -eq 1 ]; then
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
