#!/bin/sh
# Guest-side runner for the Xwayland scenario (M10).
#
# Launches weston with `--xwayland`, which tells the compositor to
# start a rootless X server (Xwayland) as a compositor-managed helper.
# Xwayland advertises a DISPLAY socket at /tmp/.X11-unix/X<N>; X11
# clients like xeyes/xterm connect to it as if to a native X server,
# but the compositor translates their windows into Wayland surfaces
# transparently.
#
# This is how modern distributions run legacy X apps on Wayland without
# asking users to install a separate X server. If this renders, we
# support arbitrary X11 clients.
set +e
export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null
export LIBSEAT_BACKEND=noop

# Weston's xwayland plugin expects /tmp/.X11-unix/ to exist (mode 1777
# per X11 convention). Our Alpine-extracted rootfs doesn't pre-populate
# the directory — systemd/elogind would normally create it on real
# systems — so we create it ourselves before weston loads the plugin.
# Without this mkdir, weston logs:
#   "failed to bind to /tmp/.X11-unix/X0: No such file or directory"
# and the plugin stays loaded but non-functional, so no X clients can
# connect.
mkdir -p /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix

# --xwayland makes weston's xwayland.so plugin load and pre-bind the
# X11 socket so clients get LAZY-spawned Xwayland on connect.
/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --xwayland \
    --no-config \
    --idle-time=0 \
    --log=/tmp/weston.log &

# Wait for weston's own wayland socket.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    DISP=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    [ -n "$DISP" ] && break
done
if [ -z "$DISP" ]; then
    echo "[fatal] weston never created a wayland socket"
    tail -30 /tmp/weston.log
    sleep 5
    poweroff -f
fi
export WAYLAND_DISPLAY="$DISP"

# Wait for weston's xwayland plugin to finish binding /tmp/.X11-unix/X0.
# The plugin init runs async after the socket is created; on TCG-riscv64
# this is ~8–15s after weston's own socket appears. Connecting too
# early gives "Can't open display :0" because the socket isn't there
# yet. We poll for the socket's existence with a generous budget.
for _ in $(seq 1 30); do
    [ -S /tmp/.X11-unix/X0 ] && break
    sleep 1
done
if [ ! -S /tmp/.X11-unix/X0 ]; then
    echo "[warn] /tmp/.X11-unix/X0 never appeared; xwayland plugin may not be loaded"
    tail -40 /tmp/weston.log
fi

# Minimal auth — Xwayland is fine with no .Xauthority, but some X apps
# fuss if XAUTHORITY is unset. An empty file satisfies the check.
touch /tmp/.Xauthority
export XAUTHORITY=/tmp/.Xauthority
export DISPLAY=:0

# First xeyes connect fans out: socket accept → weston spawns Xwayland
# → Xwayland renders → xeyes paints a ~150x150 window of googly eyes.
/usr/bin/xeyes &

# Host-side capture lands at CAPTURE_AFTER_SECS=30. Give ourselves a
# wider budget so Xwayland (~6–10s to spawn under TCG) + xeyes first
# paint both land before the snapshot.
sleep 65
poweroff -f
