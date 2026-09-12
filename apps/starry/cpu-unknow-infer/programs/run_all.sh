#!/bin/sh
# On-target launcher for the cpu-unknow-infer carpet: run the llama.cpp CPU greedy-decode
# inference correctness carpet once per model (Qwen3-0.6B and DeepSeek-R1-Distill-Qwen-1.5B)
# and gate on token-by-token equality against the committed golden. Prints "TEST PASSED" only
# when every model cell emits "INFER_LLAMACPP OK <n>" and the three-gate holds
# (fail==0 && total==EXPECTED==pass, EXPECTED = number of model cells in the manifest).
set -u
BIN=/opt/cpu-unknow-infer
export LD_LIBRARY_PATH=/opt/cpu-unknow-infer/lib:/usr/lib
# StarryOS runs a single vCPU; the carpet already pins llama.cpp to one thread and asserts
# token IDs (not throughput), so thread count does not affect the verified result.
export GGML_NTHREADS=1
export OMP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-unknow-infer: detected CPU count = $ncpu; llama.cpp pinned single-threaded"

# Manifest lines: "<cell>|<model-file>|<golden-glob>" - one per model cell. Each cell greedy-
# decodes its model against every matching golden and must print INFER_LLAMACPP OK <n>.
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-unknow-infer: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
# EXPECTED_CELLS is the hard-coded model-cell set (qwen3 + deepseek); a manifest with fewer than these 2
# means prebuild dropped a model, so the gate fails rather than shrinking EXPECTED to a partial run.
EXPECTED_CELLS=2
EXPECTED=$(grep -c . "$MANIFEST")

pass=0; total=0; fail=0; tokens_total=0
run() {
    cell="$1"; model="$2"; glob="$3"
    prog="$BIN/infer_llamacpp"
    total=$((total + 1))
    [ -x "$prog" ] || { echo "cpu-unknow-infer: $cell wanted but infer_llamacpp binary absent at runtime"; fail=$((fail + 1)); return 0; }
    [ -f "$BIN/$model" ] || { echo "cpu-unknow-infer: $cell wanted but model $model absent at runtime"; fail=$((fail + 1)); return 0; }
    set --
    for g in $BIN/golden/$glob; do [ -f "$g" ] && set -- "$@" -g "$g"; done
    [ "$#" -gt 0 ] || { echo "cpu-unknow-infer: $cell has no golden matching $glob"; fail=$((fail + 1)); return 0; }
    errf="$BIN/.$cell.stderr"
    out="$(cd "$BIN" && "$prog" -m "$BIN/$model" "$@" 2>"$errf")"; rc=$?
    n=$(echo "$out" | sed -n 's/^INFER_LLAMACPP OK \([0-9]*\)$/\1/p')
    if [ "$rc" -eq 0 ] && [ -n "$n" ]; then
        echo "[$cell / $model]"
        echo "$out" | grep -E "MATCH$|INFER_LLAMACPP OK [0-9]+$"
        pass=$((pass + 1)); tokens_total=$((tokens_total + n))
    else
        echo "[$cell / $model]"
        echo "$out" | tail -8
        # surface the per-token diagnostics infer_llamacpp writes to stderr (e.g. "token[i] got X want Y")
        # so a golden divergence is debuggable instead of being swallowed.
        [ -s "$errf" ] && { echo "--- $cell stderr (token diagnostics) ---"; tail -20 "$errf"; }
        echo "CARPET FAILED: $cell (exit $rc)"
        fail=$((fail + 1))
    fi
    rm -f "$errf"
}

while IFS= read -r line; do
    [ -n "$line" ] || continue
    cell=$(echo "$line" | cut -d'|' -f1)
    model=$(echo "$line" | cut -d'|' -f2)
    glob=$(echo "$line" | cut -d'|' -f3)
    run "$cell" "$model" "$glob"
done < "$MANIFEST"

echo "cpu-unknow-infer: $pass/$total model cells OK on $(uname -m), $tokens_total tokens verified (expected $EXPECTED_CELLS cells: $(cut -d'|' -f1 "$MANIFEST" | tr '\n' ' '))"
if [ "$EXPECTED" -eq "$EXPECTED_CELLS" ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED_CELLS" ] && [ "$pass" -eq "$EXPECTED_CELLS" ] && [ "$tokens_total" -gt 0 ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-unknow-infer: GATE FAILED - need all $EXPECTED model cells to pass (>=1 floor); got fail=$fail total=$total pass=$pass tokens=$tokens_total"
    echo "TEST FAILED"; exit 1
fi
