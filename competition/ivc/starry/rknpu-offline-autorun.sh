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
RESOURCE_PARTIAL=$RAW_DIR/resources.txt.partial
RESOURCE=$RAW_DIR/resources.txt
RESOURCE_MANIFEST=$RAW_DIR/resources.txt.sha256

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

capture_rootfs_space() {
    usage=$($BB df -Pk "$RAW_DIR" | $BB tail -n 1) \
        || halt_after_failure rootfs-space-query-failed
    set -- $usage
    [ "$#" -ge 6 ] || halt_after_failure rootfs-space-schema-mismatch
    case "$2:$4" in
        *[!0-9:]*|:*|*:) halt_after_failure rootfs-space-value-invalid ;;
    esac
    rootfs_total_kib=$2
    rootfs_available_kib=$4
    [ "$rootfs_total_kib" -gt 0 ] \
        || halt_after_failure rootfs-total-not-positive
}

[ -x "$BB" ] || halt_after_failure busybox-not-found
[ -r "$PROFILE" ] || halt_after_failure profile-not-found
. "$PROFILE"

[ "${schema:-}" = 1 ] || halt_after_failure profile-schema-mismatch
[ "${vectors:-}" = 10000 ] || halt_after_failure profile-vector-count-mismatch
[ "${warmup:-}" = 32 ] || halt_after_failure profile-warmup-mismatch
[ "${core_mask:-}" = 0 ] || halt_after_failure profile-core-mask-mismatch
[ "${lifecycle_cycles:-}" = 20 ] \
    || halt_after_failure profile-lifecycle-cycles-mismatch
[ "${maximum_post_destroy_growth_kib:-}" = 4096 ] \
    || halt_after_failure profile-memory-growth-budget-mismatch
[ "${maximum_peak_rss_kib:-}" = 524288 ] \
    || halt_after_failure profile-memory-peak-budget-mismatch
[ "${minimum_rootfs_available_percent_x100:-}" = 2000 ] \
    || halt_after_failure profile-rootfs-budget-mismatch
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

copy=0
while [ "$copy" -lt 5 ]; do
    echo "THERMAL_RKNN_STARRY_DEVICE schema=1 path=/dev/dri/card1 registered=true"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done

"$BB" mkdir -p "$RAW_DIR" || halt_after_failure raw-directory-create-failed
"$BB" rm -f \
    "$RAW_PARTIAL" "$RAW" "$RAW_MANIFEST" \
    "$RESOURCE_PARTIAL" "$RESOURCE" "$RESOURCE_MANIFEST"
capture_rootfs_space
rootfs_total_before_kib=$rootfs_total_kib
rootfs_available_before_kib=$rootfs_available_kib

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
        --evidence-marker-interval-ms 100 \
        --lifecycle-cycles "$lifecycle_cycles" \
        --resource-output "$RESOURCE_PARTIAL"; then
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
capture_rootfs_space
[ "$rootfs_total_kib" -eq "$rootfs_total_before_kib" ] \
    || halt_after_failure rootfs-total-changed
[ "$rootfs_available_kib" -le "$rootfs_total_kib" ] \
    || halt_after_failure rootfs-available-exceeds-total
rootfs_available_percent_x100=$((rootfs_available_kib * 10000 / rootfs_total_kib))
[ "$rootfs_available_percent_x100" -ge "$minimum_rootfs_available_percent_x100" ] \
    || halt_after_failure rootfs-space-budget-exceeded
"$BB" printf '%s\n' \
    "rootfs_total_kib=$rootfs_total_kib" \
    "rootfs_available_before_kib=$rootfs_available_before_kib" \
    "rootfs_available_after_kib=$rootfs_available_kib" \
    "rootfs_available_percent_x100=$rootfs_available_percent_x100" \
    >> "$RESOURCE_PARTIAL" \
    || halt_after_failure resource-rootfs-append-failed
"$BB" mv -f "$RESOURCE_PARTIAL" "$RESOURCE" \
    || halt_after_failure resource-atomic-rename-failed
resource_sha256=$(file_sha256 "$RESOURCE")
"$BB" printf '%s  %s\n' "$resource_sha256" "$RESOURCE" \
    > "$RESOURCE_MANIFEST" \
    || halt_after_failure resource-manifest-write-failed
"$BB" sync || halt_after_failure guest-filesystem-sync-failed

copy=0
while [ "$copy" -lt 5 ]; do
    echo "THERMAL_RKNN_STARRY_PASS schema=1 vectors=$vectors warmup=$warmup core_mask=$core_mask backend=rknn-npu"
    "$BB" sleep 0.1
    echo "THERMAL_RKNN_STARRY_RAW schema=1 vectors=$vectors sha256=$raw_sha256"
    "$BB" sleep 0.1
    echo "THERMAL_RKNN_STARRY_RESOURCE schema=1 lifecycle_cycles=$lifecycle_cycles sha256=$resource_sha256"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done
"$BB" sync || halt_after_failure final-sync-failed
"$BB" poweroff -f
"$BB" sleep 5
halt_after_failure poweroff-returned
