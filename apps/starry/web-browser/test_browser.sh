#!/bin/sh
set -eu

# NetSurf web-browser graphical test for StarryOS.
# Starts a Weston compositor (DRM backend + pixman software renderer) and
# launches NetSurf under Wayland to render a page, verifying the software
# browser -> virtio-gpu -> QEMU VNC pipeline end to end.

green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
reset="$(printf '\033[0m')"

weston_pid=""
httpd_pid=""
test_done=0
failed=0

fail() {
    printf "%sWEB_BROWSER_TEST_FAILED: %s%s\n" "$red" "$*" "$reset"
    echo "WEB_BROWSER_TEST_FAILED"
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

cleanup() {
    if [ -n "$weston_pid" ]; then
        kill "$weston_pid" >/dev/null 2>&1 || true
        wait "$weston_pid" >/dev/null 2>&1 || true
        weston_pid=""
    fi
    if [ -n "$httpd_pid" ]; then
        kill "$httpd_pid" >/dev/null 2>&1 || true
        httpd_pid=""
    fi
    rm -f /tmp/wayland-* 2>/dev/null || true
}

on_exit() {
    rc=$?
    cleanup
    if [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ]; then
        printf "%sWEB_BROWSER_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "WEB_BROWSER_TEST_FAILED"
    fi
    exit "$rc"
}
trap on_exit EXIT

# ---- Check pre-installed packages ----
echo "WEB_BROWSER_PREP checking pre-installed packages..."
command -v weston >/dev/null 2>&1 || fail "weston not found - prebuild may have failed"
if [ ! -x /usr/bin/netsurf ]; then
    fail "netsurf binary not found - prebuild may have failed"
fi
echo "WEB_BROWSER_PREP weston + netsurf found"

# ---- Verify DRM device ----
if [ ! -e /dev/dri/card0 ]; then
    fail "/dev/dri/card0 not found - DRM kernel driver missing"
fi
echo "WEB_BROWSER_PREP /dev/dri/card0 present"

input_count=$(ls /dev/input/event* 2>/dev/null | wc -l)
echo "WEB_BROWSER_PREP found $input_count input device(s)"

# ---- Start Weston ----
export HOME=/root
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null

# No seatd in this kernel.
export LIBSEAT_BACKEND=noop

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

echo "WEB_BROWSER_STAGE starting weston with DRM backend (pixman)..."
# --debug is what authorizes weston-screenshooter, and the gate below reads the
# page back from a capture of the composited output.
LIBGL_ALWAYS_SOFTWARE=1 /usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --debug \
    --config=/etc/xdg/weston/weston.ini \
    --idle-time=0 \
    --log=/tmp/weston.log \
    >/tmp/weston-stdout.log 2>/tmp/weston-stderr.log &
weston_pid=$!

# ---- Wait for the Wayland socket ----
socket_ready=0
for i in $(seq 1 120); do
    sleep 1
    if ! kill -0 "$weston_pid" >/dev/null 2>&1; then
        echo "WEB_BROWSER_STAGE weston exited prematurely (pid=$weston_pid)"
        tail -30 /tmp/weston.log 2>/dev/null || true
        fail "weston exited before creating Wayland socket"
    fi
    disp=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    if [ -n "$disp" ]; then
        socket_ready=1
        echo "WEB_BROWSER_STAGE Wayland socket ready: /tmp/$disp"
        break
    fi
done
if [ "$socket_ready" -ne 1 ]; then
    tail -30 /tmp/weston.log 2>/dev/null || true
    fail "weston did not create a Wayland socket within 120s"
fi

export WAYLAND_DISPLAY="$disp"
export GDK_BACKEND=wayland
export XDG_CACHE_HOME=/tmp
export FONTCONFIG_PATH=/etc/fonts
mkdir -p /tmp/fontconfig /var/cache/fontconfig 2>/dev/null || true
# The prebuild ships mime.cache and leaves no GdkPixbuf loaders.cache: GTK finds
# the decoder for its own PNG icons through the mime database, and a loader cache
# generated here would list no loaders and hide the built-in PNG one.
fc-cache -f >/dev/null 2>&1 || true
glib-compile-schemas /usr/share/glib-2.0/schemas >/dev/null 2>&1 || true

# ---- Serve the page over http, launch NetSurf, then capture ----
# NetSurf's file:// fetch left the window on about:blank, and SLIRP does not
# hairpin to the guest's own eth0 address, so the bundled page goes over loopback.
# Alpine's busybox has no httpd applet; busybox-extras does.
ip link set lo up 2>/dev/null || ifconfig lo up 2>/dev/null || true
# In the foreground with -vv, httpd logs every requested path; that log is the
# evidence of what the browser fetched.
busybox-extras httpd -f -vv -p 127.0.0.1:8080 -h /usr/share/web-browser 2>/tmp/httpd.log &
httpd_pid=$!
sleep 1
kill -0 "$httpd_pid" 2>/dev/null || fail "httpd could not listen on 127.0.0.1:8080"
page="${BROWSER_URL:-http://127.0.0.1:8080/test.html}"
if [ -z "${BROWSER_URL:-}" ]; then
    wget -q -T 10 -O /dev/null "$page" 2>/dev/null \
        || fail "the bundled page $page is unreachable over loopback"
    echo "WEB_BROWSER_STAGE serving the bundled test.html over loopback"
fi
echo "WEB_BROWSER_STAGE launching netsurf on $page ..."
/usr/bin/netsurf "$page" >/tmp/ns_stdout.log 2>/tmp/ns_err.log &
ns_pid=$!
# Give NetSurf time to lay out and paint the page onto the pixman surface, and
# hold the frame long enough for a host-side VNC capture of the virtio-gpu
# scanout (Weston's DRM output is the card0/VNC surface, not /dev/fb0).
echo "WEB_BROWSER_RENDER_WINDOW_OPEN"
sleep 40
ns_exit=0
ns_left_on_its_own=0
if ! kill -0 "$ns_pid" 2>/dev/null; then
    # It exited/crashed on its own - record its real status.
    ns_left_on_its_own=1
    wait "$ns_pid" 2>/dev/null || ns_exit=$?
fi
echo "WEB_BROWSER_DIAG === netsurf stderr ==="
head -30 /tmp/ns_err.log 2>/dev/null || true
echo "WEB_BROWSER_DIAG === netsurf stdout ==="
head -10 /tmp/ns_stdout.log 2>/dev/null || true

# Capture the composited output while NetSurf still holds the page. Weston paints
# the DRM scanout rather than /dev/fb0, so the capture has to come from Weston.
shot_dir=/tmp/shots
mkdir -p "$shot_dir"
shot=""
if [ "$ns_left_on_its_own" = 0 ]; then
    XDG_PICTURES_DIR="$shot_dir" weston-screenshooter >/tmp/screenshooter.log 2>&1 || true
    shot=$(ls "$shot_dir"/wayland-screenshot-*.png 2>/dev/null | head -n 1)
fi
if [ -n "$shot" ]; then
    echo "WEB_BROWSER_SHOT bytes=$(wc -c < "$shot")"
    echo "SCREENSHOT_PNG_BASE64_BEGIN"
    base64 "$shot" 2>/dev/null || true
    echo "SCREENSHOT_PNG_BASE64_END"
else
    echo "WEB_BROWSER_DIAG === weston-screenshooter ==="
    cat /tmp/screenshooter.log 2>/dev/null || true
fi

# NetSurf is a GUI that never exits by itself; stop it now (SIGTERM = ran ok).
if kill -0 "$ns_pid" 2>/dev/null; then
    kill "$ns_pid" 2>/dev/null || true
    ns_exit=143
fi

# 0 (clean exit), 124 (our timeout), 143 (SIGTERM) all mean it ran and rendered
# for the whole window without crashing.
if [ "$ns_exit" -eq 0 ] || [ "$ns_exit" -eq 124 ] || [ "$ns_exit" -eq 143 ]; then
    echo "WEB_BROWSER_STAGE netsurf ran without crashing (exit $ns_exit)"
else
    echo "WEB_BROWSER_STAGE netsurf exited with code $ns_exit"
    tail -20 /tmp/weston.log 2>/dev/null || true
    fail "netsurf exited with non-zero/crash code $ns_exit"
fi

# Staying up says nothing about the page. render-probe.css is linked only from
# test.html, and the wget above fetched test.html alone, so a request for the
# stylesheet means NetSurf navigated to the page and parsed it.
if [ -z "${BROWSER_URL:-}" ]; then
    echo "WEB_BROWSER_DIAG === httpd requests ==="
    grep -a "url:" /tmp/httpd.log 2>/dev/null || true
    grep -qa "url:/render-probe.css" /tmp/httpd.log \
        || fail "netsurf never requested render-probe.css: the page was not fetched and parsed"
    echo "WEB_BROWSER_STAGE netsurf fetched and parsed test.html"

    # Parsing still says nothing about pixels. test.html carries a 240x120 block
    # in #36b2a0, a colour nothing else on screen uses, so finding most of it in
    # the capture shows the page was laid out, painted and presented.
    [ -n "$shot" ] || fail "weston-screenshooter produced no capture of the output"
    probe=$(pngtopam "$shot" | pnmtoplainpnm | awk 'NR > 3 {
        for (i = 1; i <= NF; i++) {
            v[n % 3] = $i; n++
            if (n % 3 == 0 && v[0] == 54 && v[1] == 178 && v[2] == 160) c++
        }
    } END { print c + 0 }')
    echo "WEB_BROWSER_STAGE the capture holds $probe pixels of the page colour"
    [ "$probe" -ge 20000 ] \
        || fail "the page colour covers only $probe pixels of the capture: the page was not painted"
fi

# ---- Optional: try a real network fetch (diagnostic, not a pass condition) ----
echo "WEB_BROWSER_DIAG === network probe (optional) ==="
if command -v wget >/dev/null 2>&1; then
    if wget -q -T 15 -O /tmp/net_probe.html "http://example.com/" 2>/dev/null; then
        echo "WEB_BROWSER_DIAG http fetch OK ($(wc -c < /tmp/net_probe.html) bytes) - live browsing viable"
    else
        echo "WEB_BROWSER_DIAG http fetch failed (offline / proxy) - local render still valid"
    fi
fi

# ---- Weston log sanity ----
if grep -iE "failed to open|no such file|permission denied|segfault" /tmp/weston.log >/tmp/weston-errors.out 2>&1; then
    echo "WEB_BROWSER_STAGE weston log contains errors:"
    cat /tmp/weston-errors.out
else
    echo "WEB_BROWSER_STAGE no obvious errors in weston log"
fi

sleep 2
echo "WEB_BROWSER_STAGE shutting down weston..."
kill "$weston_pid" 2>/dev/null || true
for i in 1 2 3 4 5; do
    if ! kill -0 "$weston_pid" 2>/dev/null; then break; fi
    sleep 1
done
kill -9 "$weston_pid" 2>/dev/null || true
weston_pid=""

# The 143 set when this script stops NetSurf says nothing about the render, so
# the gate asks whether the browser left on its own during the hold.
if [ "$ns_left_on_its_own" = 1 ]; then
    fail "netsurf exited during the hold with status $ns_exit"
fi

test_done=1
printf "%sWEB_BROWSER_TEST_PASSED%s\n" "$green" "$reset"
echo "WEB_BROWSER_TEST_PASSED"
exit 0
