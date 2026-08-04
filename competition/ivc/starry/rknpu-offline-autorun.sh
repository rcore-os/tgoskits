#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/rknpu-offline-profile
RUNNER=/opt/thermal-rknn/thermal_rknn_reference
MODEL=/opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn
CORPUS=/opt/thermal-rknn/corpus.csv
RAW_DIR=/var/lib/rknn
RAW_PARTIAL=$RAW_DIR/raw.csv.partial
RAW=$RAW_DIR/raw.csv
RAW_MANIFEST=$RAW_DIR/raw.csv.sha256

exec >/dev/console 2>&1

halt_after_failure() {
    reason=$1
    copy=0
    while [ "$copy" -lt 3 ]; do
        echo "THERMAL_RKNN_STARRY_FAIL reason=$reason"
        copy=$((copy + 1))
        "$BB" sleep 0.1
    done
    "$BB" sync
    "$BB" poweroff -f
    while true; do
        "$BB" sleep 60
    done
}

file_sha256() {
    checksum=$($BB sha256sum "$1") || halt_after_failure sha256-failed
    printf '%s\n' "${checksum%% *}"
}

[ -x "$BB" ] || halt_after_failure busybox-not-found
[ -r "$PROFILE" ] || halt_after_failure profile-not-found
. "$PROFILE"

[ "${schema:-}" = 1 ] || halt_after_failure profile-schema-mismatch
[ "${vectors:-}" = 10000 ] || halt_after_failure profile-vector-count-mismatch
[ "${warmup:-}" = 32 ] || halt_after_failure profile-warmup-mismatch
[ "${core_mask:-}" = 0 ] || halt_after_failure profile-core-mask-mismatch
[ -x "$RUNNER" ] || halt_after_failure runner-not-found
[ -r "$MODEL" ] || halt_after_failure model-not-found
[ -r "$CORPUS" ] || halt_after_failure corpus-not-found
[ -e /dev/dri/card1 ] || halt_after_failure rknpu-device-not-found

[ "$(file_sha256 "$RUNNER")" = "$runner_sha256" ] \
    || halt_after_failure runner-hash-mismatch
[ "$(file_sha256 "$MODEL")" = "$model_sha256" ] \
    || halt_after_failure model-hash-mismatch
[ "$(file_sha256 "$CORPUS")" = "$corpus_sha256" ] \
    || halt_after_failure corpus-hash-mismatch
[ "$(file_sha256 /opt/thermal-rknn/lib/librknnrt.so)" = "$runtime_sha256" ] \
    || halt_after_failure runtime-hash-mismatch

"$BB" mkdir -p "$RAW_DIR" || halt_after_failure raw-directory-create-failed
"$BB" rm -f "$RAW_PARTIAL" "$RAW" "$RAW_MANIFEST"

copy=0
while [ "$copy" -lt 3 ]; do
    echo "THERMAL_RKNN_STARRY_BEGIN schema=1 vectors=$vectors warmup=$warmup core_mask=$core_mask backend=rknn-npu"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done
if LD_LIBRARY_PATH=/opt/thermal-rknn/lib:/lib/aarch64-linux-gnu \
    "$RUNNER" \
        --model "$MODEL" \
        --corpus "$CORPUS" \
        --output "$RAW_PARTIAL" \
        --warmup "$warmup" \
        --core-mask "$core_mask" \
        --evidence-marker-copies 5 \
        --evidence-marker-interval-ms 100; then
    run_status=0
else
    run_status=$?
fi
[ "$run_status" -eq 0 ] || halt_after_failure runner-exit-nonzero

raw_lines=$($BB wc -l < "$RAW_PARTIAL") \
    || halt_after_failure raw-line-count-failed
expected_raw_lines=10001
[ "$raw_lines" -eq "$expected_raw_lines" ] \
    || halt_after_failure raw-line-count-mismatch
"$BB" mv -f "$RAW_PARTIAL" "$RAW" \
    || halt_after_failure raw-atomic-rename-failed
raw_sha256=$(file_sha256 "$RAW")
"$BB" printf '%s  %s\n' "$raw_sha256" "$RAW" > "$RAW_MANIFEST" \
    || halt_after_failure raw-manifest-write-failed
"$BB" sync || halt_after_failure guest-filesystem-sync-failed

copy=0
while [ "$copy" -lt 5 ]; do
    echo "THERMAL_RKNN_STARRY_PASS schema=1 vectors=$vectors warmup=$warmup core_mask=$core_mask backend=rknn-npu"
    "$BB" sleep 0.1
    echo "THERMAL_RKNN_STARRY_RAW schema=1 vectors=$vectors sha256=$raw_sha256"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done
"$BB" sync || halt_after_failure final-sync-failed
"$BB" poweroff -f
"$BB" sleep 5
halt_after_failure poweroff-returned
