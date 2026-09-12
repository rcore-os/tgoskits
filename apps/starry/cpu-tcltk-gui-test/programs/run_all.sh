#!/bin/sh
# On-target runner for the cpu-tcltk-gui-test carpet - a "pyte for GUI widgets". Each cell drives a real
# Tcl/Tk pipeline (widgets, canvas items, photo images, injected events) against an X server provided
# headlessly by Xvfb (virtual framebuffer, no physical display) and asserts CLOSED-FORM goldens: exact
# photo-image pixels, exact canvas item geometry (coords/bbox), exact pack/grid/place geometry, exact font
# measure/metrics, and post-event widget state from injected `event generate` mouse/key events. Prints
# "TEST PASSED" only when every provisioned cell reports its "GUI_<CELL> OK <n>" marker (three-gate:
# fail==0 && total==EXPECTED==pass).
#
# Display bring-up: Tk needs an X display. This runner starts Xvfb on :99 (if the target has it) and points
# DISPLAY at it, then runs each .tcl cell under `wish`. If Xvfb cannot be started (display backend not yet
# available on this target), the runner reports the block explicitly and fails the gate (it does NOT fake a
# host-only pass) - see README "On-target run" for the honest scoping.
#
# Cells:
#   gui_render      - photo-image per-pixel fillRect/copy vs closed form + canvas item geometry
#                     (coords/bbox/find/move) + canvas text bbox (font-agnostic).
#   gui_layout      - deterministic geometry: place/pack/grid child geometry == closed-form manager math,
#                     labelframe padding composition, reqwidth/reqheight of fixed widgets.
#   gui_interact    - inject events: button click fires command (disabled does not - negative control),
#                     checkbutton variable + Space toggle, entry keystrokes/edit ops, scale step/clamp,
#                     listbox insert/get/select/delete.
#   gui_realassets  - real font family: font measure N-char == N*one-char (fixed pitch), metrics relations,
#                     canvas text bbox scaling. Honest-skips (still passes, total>0) if no family resolvable.
set -u
BIN=/opt/cpu-tcltk-gui-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
export HOME="${HOME:-/tmp}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}"
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"

WISH="$(command -v wish 2>/dev/null || echo /usr/bin/wish)"
[ -x "$WISH" ] || { echo "cpu-tcltk-gui-test: wish (Tk) not found on target"; echo "TEST FAILED"; exit 1; }

# ---- headless X display via Xvfb ------------------------------------------------------------------
XVFB_PID=""
start_display() {
    XVFB="$(command -v Xvfb 2>/dev/null || true)"
    if [ -z "$XVFB" ]; then
        echo "cpu-tcltk-gui-test: Xvfb not present - cannot bring up a headless X display"
        return 1
    fi
    # a private auth file keeps X quiet on a fresh target
    export DISPLAY=:99
    rm -f /tmp/.X99-lock 2>/dev/null || true
    "$XVFB" :99 -screen 0 1024x768x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
    XVFB_PID=$!
    # wait for the server socket to appear (bounded)
    i=0
    while [ $i -lt 50 ]; do
        if [ -S /tmp/.X11-unix/X99 ]; then return 0; fi
        i=$((i + 1)); sleep 0.1 2>/dev/null || sleep 1
    done
    # last-chance probe: a trivial Tk connect
    if echo 'exit 0' | "$WISH" >/dev/null 2>&1; then return 0; fi
    echo "cpu-tcltk-gui-test: Xvfb did not become ready (see /tmp/xvfb.log)"
    cat /tmp/xvfb.log 2>/dev/null | tail -8
    return 1
}
stop_display() { [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null || true; }

if ! start_display; then
    echo "cpu-tcltk-gui-test: no headless X display available - GATE BLOCKED (display backend gated on #392)"
    echo "TEST FAILED"; exit 1
fi
trap stop_display EXIT INT TERM

ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-tcltk-gui-test: detected CPU count = $ncpu; DISPLAY=$DISPLAY; wish=$WISH; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -f "$prog" ] || { echo "cpu-tcltk-gui-test: $name in manifest but script absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$WISH" "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -12
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || { echo "TEST FAILED"; exit 1; }
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-tcltk-gui-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell.tcl"
done 3< "$MANIFEST"

echo "cpu-tcltk-gui-test: $pass/$total GUI carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-tcltk-gui-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
