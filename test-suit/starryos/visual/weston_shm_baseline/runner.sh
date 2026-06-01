#!/bin/sh
# Guest-side runner for the "weston desktop + weston-simple-shm" scenario.
# Expected rootfs contents: /usr/bin/weston, /usr/bin/weston-simple-shm,
# /usr/lib/libweston-14/drm-backend.so, and all transitive .so deps. The
# rootfs-builder manifests (scripts/rootfs-builder/manifests/m6-*.yaml)
# capture the full set from Alpine edge.
set +e
export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null
# Skip libseat session management — our kernel doesn't run seatd and
# we don't need per-seat fd delegation for a single-user visual test.
export LIBSEAT_BACKEND=noop

/usr/bin/weston \
    --backend=drm-backend.so \
    --renderer=pixman \
    --no-config \
    --idle-time=0 \
    --log=/tmp/weston.log &
WESTON_PID=$!

# Wait for the compositor to create its wayland-N socket.
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

# NO animated clients — the diff needs to be deterministic. Weston's
# idle desktop (panel + background pattern) is the regression signal:
# any breakage in DRM/compositor path changes it visibly.
# `--idle-time=0` prevents weston from fading the panel after ~300s.
# The panel clock still ticks once a minute; the diff tolerance
# absorbs that single-minute churn (one digit → ~3px of change).

# Stay alive long enough for the harness to capture at t=25s. Shorter
# than that and we race the host; longer burns CI time unnecessarily.
sleep 50
poweroff -f
