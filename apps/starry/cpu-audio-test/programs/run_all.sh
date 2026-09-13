#!/bin/sh
# On-target runner for the cpu-audio-test carpet - the "pyte for audio". Each cell decodes audio to
# in-memory PCM and asserts in the SIGNAL domain (FFT bins / RMS / SNR / THD+N / PSNR / byte-exact
# SHA-256) against an analytically-known or golden reference. Prints "TEST PASSED" only when every
# provisioned cell reports its "AUDIO_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass).
#
# Cells:
#   audio_fft        - synthetic known-signal spectral leg (sine peak bin+mag+SNR, DTMF, chirp, silence,
#                      impulse, THD+N, channel separation, DC offset, clipping). No ffmpeg needed.
#   audio_codec      - codec cartesian {wav,flac,opus,aac,mp3} x {mono,stereo} x {44100,48000}: encode ->
#                      decode -> lossless byte-exact SHA / lossy FFT-peak+PSNR, metadata exact.
#   audio_resample   - 44100<->48000 FFT-peak migration + anti-alias (above-Nyquist tone removed).
#   audio_realassets - decode the real media submodule + assert decoded stats == golden tsv; honest-skip
#                      when $ASSET_DIR is absent (the synthetic legs always gate).
set -u
BIN=/opt/cpu-audio-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
# Real-asset dir: on-target the media submodule mounts here; default keeps the synthetic legs gating even
# if it is absent (audio_realassets honest-skips).
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-audio-test: detected CPU count = $ncpu; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-audio-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
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
[ -f "$MANIFEST" ] || { echo "cpu-audio-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-audio-test: $pass/$total audio carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-audio-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
