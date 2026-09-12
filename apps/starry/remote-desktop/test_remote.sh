#!/bin/sh
set -eu

# Headless remote desktop test for StarryOS with NO GPU device.
# Xvfb provides a pure software X framebuffer (no /dev/dri); feh paints a
# banner into it; x11vnc exports the framebuffer over VNC. QEMU forwards a
# host port to the guest's x11vnc, so the host can connect and see the GUI -
# a remote desktop rendered entirely on the CPU.

green="$(printf '\033[32m')"
red="$(printf '\033[31m')"
reset="$(printf '\033[0m')"

xvfb_pid=""
vnc_pid=""
feh_pid=""
test_done=0
failed=0

fail() {
    printf "%sREMOTE_DESKTOP_TEST_FAILED: %s%s\n" "$red" "$*" "$reset"
    echo "REMOTE_DESKTOP_TEST_FAILED"
    failed=1
    exit 1
}

cleanup() {
    for p in "$feh_pid" "$vnc_pid" "$xvfb_pid"; do
        [ -n "$p" ] && kill "$p" >/dev/null 2>&1 || true
    done
    rm -f /tmp/.X99-lock 2>/dev/null || true
}

on_exit() {
    rc=$?
    cleanup
    if [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ]; then
        printf "%sREMOTE_DESKTOP_TEST_RESULT FAILED%s\n" "$red" "$reset"
        echo "REMOTE_DESKTOP_TEST_FAILED"
    fi
    exit "$rc"
}
trap on_exit EXIT

echo "REMOTE_DESKTOP_PREP checking pre-installed packages..."
command -v Xvnc >/dev/null 2>&1 || fail "Xvnc not found - prebuild may have failed"
command -v feh  >/dev/null 2>&1 || fail "feh not found - prebuild may have failed"
command -v xterm >/dev/null 2>&1 || fail "xterm not found - prebuild may have failed"
echo "REMOTE_DESKTOP_PREP Xvnc + feh + xterm found"

# Prove there is no DRM/GPU device: the whole point is software-only display.
if [ -e /dev/dri/card0 ]; then
    echo "REMOTE_DESKTOP_PREP note: /dev/dri/card0 present but unused (software fb)"
else
    echo "REMOTE_DESKTOP_PREP no /dev/dri present - pure software framebuffer"
fi

export HOME=/root
export XDG_RUNTIME_DIR=/tmp
mkdir -p /tmp/.X11-unix 2>/dev/null || true
chmod 1777 /tmp/.X11-unix 2>/dev/null || true
export FONTCONFIG_PATH=/etc/fonts
mkdir -p /tmp/fontconfig 2>/dev/null || true
fc-cache -f >/dev/null 2>&1 || true

# ---- Software X framebuffer + VNC in one, NO GPU ----
# Xvnc (TigerVNC) is an X server whose framebuffer lives in RAM (no /dev/dri)
# and which speaks VNC directly on -rfbport. This avoids x11vnc entirely: there
# is no separate VNC client doing XOpenDisplay, only the X server itself, so the
# whole remote-desktop pipeline is a single software process.
echo "REMOTE_DESKTOP_STAGE starting Xvnc :99 (software fb 1024x768x24 + VNC 5900, no GPU)..."
# Point Xvnc at the installed X fonts and let it fall back to built-in fonts so
# a missing default 'fixed' font does not abort startup.
xvnc_fp="/usr/share/fonts/misc,/usr/share/fonts/dejavu,/usr/share/fonts/Type1,built-ins"
for d in /usr/share/fonts/misc /usr/share/fonts/dejavu; do
    [ -d "$d" ] && command -v mkfontdir >/dev/null 2>&1 && mkfontdir "$d" >/dev/null 2>&1 || true
done
# -UseIPv6=0: TigerVNC otherwise binds IPv4 then IPv6 on the same port, and
# StarryOS's dual-stack bind reports EADDRINUSE on the second bind, aborting.
Xvnc :99 -geometry 1024x768 -depth 24 -rfbport 5900 -SecurityTypes None \
    -AlwaysShared -desktop starry -localhost 0 -UseIPv6=0 -fp "$xvnc_fp" -verbose \
    >/tmp/xvnc.log 2>&1 &
xvfb_pid=$!
export DISPLAY=:99

socket_ready=0
for i in $(seq 1 60); do
    sleep 1
    if ! kill -0 "$xvfb_pid" >/dev/null 2>&1; then
        echo "REMOTE_DESKTOP_DIAG === Xvnc log (exited) ==="
        cat /tmp/xvnc.log 2>/dev/null || true
        fail "Xvnc exited before creating its X socket"
    fi
    if [ -S /tmp/.X11-unix/X99 ]; then
        socket_ready=1
        echo "REMOTE_DESKTOP_STAGE X socket ready: /tmp/.X11-unix/X99"
        break
    fi
done
if [ "$socket_ready" -ne 1 ]; then
    echo "REMOTE_DESKTOP_DIAG === Xvnc log (no socket) ==="
    cat /tmp/xvnc.log 2>/dev/null || true
    fail "Xvnc did not create its X socket within 60s"
fi

# Explicitly prove whether ANY X client can connect to :99 (isolates an
# X-server/socket problem from an x11vnc-specific one).
sleep 1
xsetroot -display :99 -solid '#113355' >/tmp/xtest.log 2>&1
echo "REMOTE_DESKTOP_DIAG xsetroot -display :99 rc=$? (can an X client reach :99?)"
cat /tmp/xtest.log 2>/dev/null || true
echo "REMOTE_DESKTOP_DIAG === Xvfb log so far ==="
head -25 /tmp/xvfb.log 2>/dev/null || true

# Backdrop banner as the root pixmap, a window manager, and an interactive
# terminal on top. The terminal is the interaction target: input arriving over
# VNC (host -> x11vnc -> XTEST -> X) is typed into it, proving the remote
# desktop is interactive, not just a static image.
xsetroot -solid '#20446c' >/dev/null 2>&1 || true
if [ -f /usr/share/remote-desktop/banner.png ]; then
    feh --no-fehbg --bg-scale /usr/share/remote-desktop/banner.png >/tmp/feh.log 2>&1 || true
    echo "REMOTE_DESKTOP_STAGE banner set as desktop background"
else
    echo "REMOTE_DESKTOP_STAGE banner missing, solid backdrop only"
fi
twm >/tmp/twm.log 2>&1 &
twm_pid=$!
sleep 1
xterm -geometry 80x24+120+180 -fn fixed -bg black -fg green \
    -T starry-term -e /bin/sh >/tmp/xterm.log 2>&1 &
xterm_pid=$!
echo "REMOTE_DESKTOP_STAGE launched twm + xterm (interaction target)"
echo "REMOTE_DESKTOP_MARK A after-xterm"
sleep 4
echo "REMOTE_DESKTOP_MARK B after-sleep4"
# eth0 is already up via DHCP; do NOT run `ip link set` (its netlink SET path
# can hang on StarryOS) and do NOT touch lo (loopback ops hang here too).
# A bare listener on a second forwarded port, so a host that cannot reach Xvnc
# can tell which half is at fault: reaching this one means inbound connections
# arrive and the problem is Xvnc's, reaching neither means they do not.
if command -v nc >/dev/null 2>&1; then
    (while :; do echo "STARRY-INBOUND-PROBE-OK" | nc -l -p 5910 >/dev/null 2>&1 || sleep 1; done) &
    echo "REMOTE_DESKTOP_STAGE inbound probe listening on 5910"
else
    echo "REMOTE_DESKTOP_DIAG no nc, inbound probe skipped"
fi

echo "REMOTE_DESKTOP_MARK C before-xvnclog"
grep -iE "listen|rfbport|port [0-9]+|error|fatal|fail" /tmp/xvnc.log 2>/dev/null | head -12 || true
echo "REMOTE_DESKTOP_MARK D after-xvnclog"

# ---- Reverse VNC: connect OUT to a host-side viewer through the SLIRP gateway.
# StarryOS accepts outbound connections (proven) but not inbound hostfwd, so
# instead of the host connecting in, the guest's Xvnc connects out to a viewer
# listening on the host at 10.0.2.2:5500 (the QEMU user-net gateway maps to the
# host). vncconfig -connect tells the running Xvnc to make that reverse link.
rev_host="${REMOTE_DESKTOP_REVERSE_HOST:-10.0.2.2}"
rev_port="${REMOTE_DESKTOP_REVERSE_PORT:-5500}"
echo "REMOTE_DESKTOP_STAGE reverse-connecting Xvnc to ${rev_host}:${rev_port} ..."
for attempt in 1 2 3 4 5 6; do
    DISPLAY=:99 vncconfig -connect "${rev_host}:${rev_port}" >/tmp/vncconfig.log 2>&1 \
        && { echo "REMOTE_DESKTOP_STAGE reverse-connect request sent (attempt $attempt)"; break; }
    sleep 3
done
cat /tmp/vncconfig.log 2>/dev/null || true

# Confirm mapped windows exist on the software display (content present).
if command -v xwininfo >/dev/null 2>&1; then
    xwininfo -root -display :99 -children 2>/dev/null | head -20 || true
fi

# Hold the frame so the host can connect through the QEMU hostfwd and capture.
echo "REMOTE_DESKTOP_WINDOW_OPEN"
sleep 90

echo "REMOTE_DESKTOP_DIAG === Xvnc log tail ==="
tail -20 /tmp/xvnc.log 2>/dev/null || true

# The point of this app is that the desktop is reachable over VNC, so the log
# has to say Xvnc is listening for that to be true. Holding a frame for ninety
# seconds proves only that nothing crashed.
if grep -qiE "Listening for VNC|rfbport|port 5900" /tmp/xvnc.log 2>/dev/null; then
    echo "REMOTE_DESKTOP_STAGE Xvnc reports its RFB port is listening"
else
    fail "Xvnc never reported an RFB listener; the desktop is not reachable"
fi

test_done=1
printf "%sREMOTE_DESKTOP_TEST_PASSED%s\n" "$green" "$reset"
echo "REMOTE_DESKTOP_TEST_PASSED"
exit 0
