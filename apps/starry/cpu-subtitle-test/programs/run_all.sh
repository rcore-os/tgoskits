#!/bin/sh
# On-target runner for the cpu-subtitle-test carpet - a "pyte for subtitles". Each cell drives a real
# subtitle pipeline the carpet implements itself (SubRip/.srt, WebVTT/.vtt, Advanced SubStation Alpha/.ass
# parsers + a cross-format timing converter) and asserts the output against a DETERMINISTIC golden:
# synthetic cues authored in-code (every timestamp known - 00:00:01,000 -> 1000 ms; 01:02:03,456 ->
# 3723456 ms; ASS H:MM:SS.cc centiseconds -> ms; SRT<->VTT round-trip preserving ms exactly) or a
# STRUCTURAL golden computed host-side from the real files (cue/dialogue count, first/last timestamp,
# monotonic ordering, valid UTF-8 - never the dialogue text). Prints "TEST PASSED" only when every
# provisioned cell reports its "SUBTITLE_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass).
#
# Cells:
#   subtitle_srt        - SubRip parser; synthetic cues: cue count, exact-ms start/end, index sequence,
#                         monotonic non-overlap, multi-line join, duration, BOM/CRLF/trailing-blank/empty edge cases.
#   subtitle_ass        - ASS parser; [Script Info]/[V4+ Styles]/[Events]; Dialogue count, centisecond->ms
#                         Start/End, Style/Layer, Format-column ordering honored, style table (Name/Fontname/
#                         Fontsize/PrimaryColour &HAABBGGRR), override-tag stripping to plain-text length.
#   subtitle_vtt        - WebVTT parser; WEBVTT header enforced, NOTE blocks skipped, HH:MM:SS.mmm + MM:SS.mmm,
#                         cue settings/identifier tolerated.
#   subtitle_convert    - SRT<->VTT timing round-trip: comma vs dot separator, ms preserved exactly; optional
#                         ffmpeg srt->vtt structural cross-check (honest-skip if ffmpeg absent).
#   subtitle_realassets - parse the real .srt/.ass, assert STRUCTURE only (counts / first-last ts / monotonic /
#                         within [0,media] / UTF-8 / style-layer uniformity) vs host-computed golden. The
#                         prebuild always stages the assets, so their absence is a hard FAIL, not a skip.
set -u
BIN=/opt/cpu-subtitle-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
# Asset dir: the prebuild stages the real subtitles here from the media submodule; on-target the submodule
# may also mount at ASSET_DIR. The prebuild aborts if the assets are absent, so this dir is always populated.
export SUBTITLE_DIR="${SUBTITLE_DIR:-$BIN/assets}"
export ASSET_DIR="${ASSET_DIR:-$SUBTITLE_DIR}"
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-subtitle-test: detected CPU count = $ncpu; SUBTITLE_DIR=$SUBTITLE_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-subtitle-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-subtitle-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-subtitle-test: $pass/$total subtitle carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-subtitle-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
