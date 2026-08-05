#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/ort-offline-profile
RUNNER=/opt/thermal-ort/thermal_ort_reference
MODEL=/opt/thermal-ort/thermal-4x6x1-v1.ort
CORPUS=/opt/thermal-ort/corpus.csv
RUNTIME=/opt/thermal-ort/lib/libonnxruntime.so.1
PROVIDER_SHARED=/opt/thermal-ort/lib/libonnxruntime_providers_shared.so
RAW_DIR=/var/lib/ort
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
        echo "THERMAL_ORT_STARRY_FAIL reason=$reason"
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
[ "${lifecycle_cycles:-}" = 5 ] \
    || halt_after_failure profile-lifecycle-cycles-mismatch
[ "${runtime_version:-}" = 1.25.0 ] \
    || halt_after_failure profile-runtime-version-mismatch
[ "${maximum_post_destroy_growth_kib:-}" = 16384 ] \
    || halt_after_failure profile-memory-growth-budget-mismatch
[ "${maximum_peak_rss_kib:-}" = 131072 ] \
    || halt_after_failure profile-memory-peak-budget-mismatch
[ "${minimum_rootfs_available_percent_x100:-}" = 2000 ] \
    || halt_after_failure profile-rootfs-budget-mismatch
[ -x "$RUNNER" ] || halt_after_failure runner-not-found
[ -r "$MODEL" ] || halt_after_failure model-not-found
[ -r "$CORPUS" ] || halt_after_failure corpus-not-found
[ -r "$RUNTIME" ] || halt_after_failure runtime-not-found
[ -r "$PROVIDER_SHARED" ] || halt_after_failure provider-shared-not-found

[ "$(file_sha256 "$RUNNER")" = "$runner_sha256" ] \
    || halt_after_failure runner-hash-mismatch
[ "$(file_sha256 "$MODEL")" = "$model_sha256" ] \
    || halt_after_failure model-hash-mismatch
[ "$(file_sha256 "$CORPUS")" = "$corpus_sha256" ] \
    || halt_after_failure corpus-hash-mismatch
[ "$(file_sha256 "$RUNTIME")" = "$runtime_sha256" ] \
    || halt_after_failure runtime-hash-mismatch
[ "$(file_sha256 "$PROVIDER_SHARED")" = "$provider_shared_sha256" ] \
    || halt_after_failure provider-shared-hash-mismatch

"$BB" mkdir -p "$RAW_DIR" || halt_after_failure raw-directory-create-failed
"$BB" rm -f \
    "$RAW_PARTIAL" "$RAW" "$RAW_MANIFEST" \
    "$RESOURCE_PARTIAL" "$RESOURCE" "$RESOURCE_MANIFEST"
capture_rootfs_space
rootfs_total_before_kib=$rootfs_total_kib
rootfs_available_before_kib=$rootfs_available_kib

copy=0
while [ "$copy" -lt 3 ]; do
    echo "THERMAL_ORT_STARRY_BEGIN schema=1 vectors=$vectors warmup=$warmup backend=onnxruntime-cpu"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done
if LD_LIBRARY_PATH=/opt/thermal-ort/lib:/lib/aarch64-linux-gnu \
    "$RUNNER" \
        --model "$MODEL" \
        --corpus "$CORPUS" \
        --output "$RAW_PARTIAL" \
        --resource-output "$RESOURCE_PARTIAL" \
        --warmup "$warmup" \
        --lifecycle-cycles "$lifecycle_cycles"; then
    run_status=0
else
    run_status=$?
fi
[ "$run_status" -eq 0 ] || halt_after_failure runner-exit-nonzero

raw_lines=$($BB wc -l < "$RAW_PARTIAL") \
    || halt_after_failure raw-line-count-failed
[ "$raw_lines" -eq 10001 ] || halt_after_failure raw-line-count-mismatch
[ -r "$RESOURCE_PARTIAL" ] || halt_after_failure resource-output-not-found
. "$RESOURCE_PARTIAL"
[ "${schema:-}" = 1 ] || halt_after_failure resource-schema-mismatch
[ "${backend:-}" = onnxruntime-cpu ] || halt_after_failure resource-backend-mismatch
[ "${runtime_version:-}" = 1.25.0 ] || halt_after_failure resource-runtime-mismatch
[ "${lifecycle_cycles:-}" = 5 ] || halt_after_failure resource-lifecycle-mismatch
[ "${exact_actuator_matches:-}" = 9999 ] \
    || halt_after_failure resource-exact-command-mismatch
[ "${rounding_boundary_equivalences:-}" = 1 ] \
    || halt_after_failure resource-rounding-equivalence-mismatch
[ "${material_actuator_mismatches:-}" = 0 ] \
    || halt_after_failure resource-material-mismatch
case "${rss_first_after_destroy_kib:-}:${rss_after_main_destroy_kib:-}:${peak_rss_kib:-}" in
    *[!0-9:]*|:*|*:) halt_after_failure resource-memory-value-invalid ;;
esac
if [ "$rss_after_main_destroy_kib" -ge "$rss_first_after_destroy_kib" ]; then
    post_destroy_growth_kib=$((rss_after_main_destroy_kib - rss_first_after_destroy_kib))
else
    post_destroy_growth_kib=0
fi
[ "$post_destroy_growth_kib" -le "$maximum_post_destroy_growth_kib" ] \
    || halt_after_failure resource-memory-growth-budget-exceeded
[ "$peak_rss_kib" -le "$maximum_peak_rss_kib" ] \
    || halt_after_failure resource-memory-peak-budget-exceeded

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
    "post_destroy_growth_kib=$post_destroy_growth_kib" \
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
    echo "THERMAL_ORT_STARRY_PASS schema=1 vectors=$vectors backend=onnxruntime-cpu"
    "$BB" sleep 0.1
    echo "THERMAL_ORT_STARRY_RAW schema=1 vectors=$vectors sha256=$raw_sha256"
    "$BB" sleep 0.1
    echo "THERMAL_ORT_STARRY_RESOURCE schema=1 cycles=$lifecycle_cycles sha256=$resource_sha256"
    "$BB" sleep 0.1
    echo "THERMAL_ORT_STARRY_RUNTIME version=$runtime_version model_sha256=$model_sha256"
    "$BB" sleep 0.1
    echo "THERMAL_ORT_STARRY_RESULT schema=1 vectors=$vectors max_abs_error=$maximum_absolute_error exact_commands=$exact_actuator_matches rounding_equivalences=$rounding_boundary_equivalences material_mismatches=$material_actuator_mismatches init_us=$initialization_us wall_p99_ns=$wall_p99_ns wall_max_ns=$wall_max_ns"
    copy=$((copy + 1))
    "$BB" sleep 0.1
done
"$BB" sync || halt_after_failure final-sync-failed
"$BB" poweroff -f
"$BB" sleep 5
halt_after_failure poweroff-returned
