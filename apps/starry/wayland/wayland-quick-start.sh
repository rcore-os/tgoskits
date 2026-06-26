#!/bin/sh
set -eu

fail() {
    echo "WAYLAND_QUICK_START_FAILED: $*"
    exit 1
}

echo "WAYLAND_QUICK_START begin"

if ! command -v weston >/dev/null 2>&1; then
    fail "weston not found; run the Wayland app prebuild/provision flow first"
fi
if ! command -v gtk4-demo >/dev/null 2>&1; then
    fail "gtk4-demo not found; rootfs is missing gtk4.0-demo"
fi

export XDG_RUNTIME_DIR=/tmp
chmod 0700 /tmp
export LIBSEAT_BACKEND=noop
rm -f /tmp/wayland-* 2>/dev/null || true

weston \
  --backend=drm-backend.so \
  --renderer=pixman \
  --no-config \
  --idle-time=0 \
  --log=/tmp/weston.log &
weston_pid=$!
echo "WAYLAND_QUICK_START weston pid=$weston_pid"

display=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 1
    if ! kill -0 "$weston_pid" >/dev/null 2>&1; then
        tail -80 /tmp/weston.log 2>/dev/null || true
        fail "weston exited before creating a Wayland socket"
    fi
    display="$(basename "$(ls /tmp/wayland-* 2>/dev/null | head -1)" 2>/dev/null || true)"
    if [ -n "$display" ]; then
        break
    fi
done

if [ -z "$display" ]; then
    tail -80 /tmp/weston.log 2>/dev/null || true
    fail "weston did not create /tmp/wayland-*"
fi

export WAYLAND_DISPLAY="$display"
export GDK_BACKEND=wayland
export GSK_RENDERER=cairo

echo "WAYLAND_QUICK_START display=$WAYLAND_DISPLAY"
gtk4-demo &
gtk_pid=$!
echo "WAYLAND_QUICK_START gtk4-demo pid=$gtk_pid"

sleep 2
ps | grep gtk4-demo | grep -v grep || fail "gtk4-demo process not found"
echo "WAYLAND_QUICK_START_READY"

# Keep the QEMU/VNC session alive for interaction even if gtk4-demo exits.
while :; do
    sleep 3600
done
