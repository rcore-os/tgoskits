#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/ivc-profile
restart_marker=/var/lib/ivc/restart-phase-1.done
restart_raw_csv=/var/lib/ivc/raw-before-reset.csv
restart_controller_log=/var/lib/ivc/controller-before-reset.log

exec >/dev/console 2>&1

fatal() {
    echo "IVC-STARRY-FATAL reason=$1"
    "$BB" sync
    "$BB" poweroff -f
    while true; do
        "$BB" sleep 60
    done
}

[ -x "$BB" ] || fatal busybox-not-found
[ -r "$PROFILE" ] || fatal profile-not-found
. "$PROFILE"

case "${ivc_mode:-}" in
    neural|manual) ;;
    *) fatal invalid-controller-mode ;;
esac
case "${ivc_backend:-}" in
    native|rknn-npu) ;;
    onnxruntime) fatal onnxruntime-backend-not-installed ;;
    *) fatal invalid-inference-backend ;;
esac
case "${ivc_fault_profile:-none}" in
    none|error|restart) ;;
    *) fatal invalid-fault-profile ;;
esac
case "${ivc_count:-}" in
    ''|*[!0-9]*) fatal invalid-command-count ;;
esac
case "${ivc_period_ms:-}" in
    ''|*[!0-9]*) fatal invalid-period ;;
esac
[ "${ivc_ack_timeout_ms:-}" = 1000 ] || fatal invalid-ack-timeout
[ "${ivc_raw_csv:-}" = /var/lib/ivc/raw.csv ] || fatal invalid-raw-csv-path
if [ "$ivc_backend" = rknn-npu ]; then
    [ "${ivc_mode:-}" = neural ] || fatal rknn-requires-neural-mode
    [ "${ivc_fault_profile:-none}" = none ] || fatal rknn-fault-profile-unsupported
    [ "${ivc_rknn_model:-}" = /opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn ] \
        || fatal invalid-rknn-model-path
    [ "${ivc_rknn_runtime:-}" = /usr/local/bin/lib/librknnrt.so ] \
        || fatal invalid-rknn-runtime-path
    [ "${ivc_rknn_evidence:-}" = /var/lib/ivc/rknn.csv ] \
        || fatal invalid-rknn-evidence-path
    [ "${ivc_rknn_core_mask:-}" = 0 ] || fatal invalid-rknn-core-mask
    for expected_hash in \
        "${ivc_rknn_model_sha256:-}" \
        "${ivc_rknn_runtime_sha256:-}" \
        "${ivc_rknn_controller_sha256:-}"; do
        case "$expected_hash" in
            ''|*[!0-9a-f]*) fatal invalid-rknn-profile-hash ;;
            *) ;;
        esac
        [ "${#expected_hash}" -eq 64 ] || fatal invalid-rknn-profile-hash-length
    done
fi
if [ "${ivc_fault_profile:-none}" = restart ]; then
    [ "${ivc_restart_previous_session:-}" = 286331153 ] \
        || fatal invalid-restart-previous-session
    [ "${ivc_restart_current_session:-}" = 572662306 ] \
        || fatal invalid-restart-current-session
    [ "${ivc_restart_first_count:-}" = 20 ] || fatal invalid-restart-first-count
    [ "${ivc_restart_ack_timeout_ms:-}" = 1000 ] \
        || fatal invalid-restart-ack-timeout
    [ "$ivc_count" = 100 ] || fatal invalid-restart-final-count
fi

validate_raw_csv() {
    raw_path=$1
    expected_samples=$2
    [ -r "$raw_path" ] || fatal raw-csv-not-found
    raw_lines=$("$BB" wc -l < "$raw_path") || fatal raw-csv-count-failed
    expected_raw_lines=$((expected_samples + 1))
    [ "$raw_lines" -eq "$expected_raw_lines" ] || fatal raw-csv-count-mismatch
    raw_checksum=$("$BB" sha256sum "$raw_path") || fatal raw-csv-hash-failed
    validated_raw_sha256=${raw_checksum%% *}
    raw_manifest=$raw_path.sha256
    "$BB" printf '%s  %s\n' "$validated_raw_sha256" "$raw_path" >"$raw_manifest" \
        || fatal raw-manifest-write-failed
}

validate_rknn_evidence() {
    [ -r "$ivc_rknn_evidence" ] || fatal rknn-evidence-not-found
    rknn_header=$($BB sed -n '1p' "$ivc_rknn_evidence") \
        || fatal rknn-evidence-header-failed
    [ "$rknn_header" = sequence,input0_bits,input1_bits,input2_bits,input3_bits,output_bits,actuator_permille,wall_ns,device_us ] \
        || fatal rknn-evidence-header-mismatch
    rknn_lines=$($BB wc -l < "$ivc_rknn_evidence") \
        || fatal rknn-evidence-count-failed
    expected_rknn_lines=$((ivc_count + 1))
    [ "$rknn_lines" -eq "$expected_rknn_lines" ] \
        || fatal rknn-evidence-count-mismatch
    rknn_checksum=$($BB sha256sum "$ivc_rknn_evidence") \
        || fatal rknn-evidence-hash-failed
    validated_rknn_sha256=${rknn_checksum%% *}
    "$BB" printf '%s  %s\n' "$validated_rknn_sha256" "$ivc_rknn_evidence" \
        >"$ivc_rknn_evidence.sha256" || fatal rknn-manifest-write-failed
}

cpu_count=$($BB grep -c '^processor' /proc/cpuinfo 2>/dev/null || true)
[ "$cpu_count" -ge 2 ] || fatal insufficient-vcpus

boot_identity="IVC-STARRY-BOOT mode=$ivc_mode backend=$ivc_backend fault_profile=${ivc_fault_profile:-none} count=$ivc_count period_ms=$ivc_period_ms vcpus=$cpu_count"
if [ "$ivc_backend" = rknn-npu ]; then
    boot_identity_copy=0
    while [ "$boot_identity_copy" -lt 5 ]; do
        echo "$boot_identity"
        boot_identity_copy=$((boot_identity_copy + 1))
        "$BB" sleep 0.025
    done
else
    echo "$boot_identity"
fi

if [ "$ivc_backend" = rknn-npu ]; then
    [ -c /dev/dri/card1 ] || fatal rknpu-device-not-found
    actual_rknn_model_sha256=$($BB sha256sum "$ivc_rknn_model") \
        || fatal rknn-model-hash-failed
    actual_rknn_model_sha256=${actual_rknn_model_sha256%% *}
    [ "$actual_rknn_model_sha256" = "$ivc_rknn_model_sha256" ] \
        || fatal rknn-model-hash-mismatch
    actual_rknn_runtime_sha256=$($BB sha256sum "$ivc_rknn_runtime") \
        || fatal rknn-runtime-hash-failed
    actual_rknn_runtime_sha256=${actual_rknn_runtime_sha256%% *}
    [ "$actual_rknn_runtime_sha256" = "$ivc_rknn_runtime_sha256" ] \
        || fatal rknn-runtime-hash-mismatch
    actual_rknn_controller_sha256=$($BB sha256sum /usr/local/bin/ivcproto) \
        || fatal rknn-controller-hash-failed
    actual_rknn_controller_sha256=${actual_rknn_controller_sha256%% *}
    [ "$actual_rknn_controller_sha256" = "$ivc_rknn_controller_sha256" ] \
        || fatal rknn-controller-hash-mismatch
    device_copy=0
    while [ "$device_copy" -lt 5 ]; do
        echo "IVC-STARRY-RKNN-DEVICE path=/dev/dri/card1 registered=true core_mask=$ivc_rknn_core_mask"
        device_copy=$((device_copy + 1))
        "$BB" sleep 0.025
    done
    "$BB" rm -f "$ivc_rknn_evidence" "$ivc_rknn_evidence.sha256" \
        || fatal rknn-stale-evidence-cleanup-failed
fi

attempt=0
while [ "$attempt" -lt 60 ]; do
    if "$BB" ip link show dev eth0 >/dev/null 2>&1; then
        break
    fi
    attempt=$((attempt + 1))
    "$BB" sleep 1
done
[ "$attempt" -lt 60 ] || fatal eth0-not-found

# Starry brings the kernel-owned interface up during ax_net initialization.
# BusyBox implements `ip link set` through SIOCSIFFLAGS, which Starry does not
# expose for this interface; address configuration uses the supported rtnetlink
# path and does not require toggling the already-active link.
"$BB" ip addr flush dev eth0 >/dev/null 2>&1 || true
"$BB" ip addr add 10.0.0.1/24 dev eth0 || fatal eth0-address-failed

mac=$($BB cat /sys/class/net/eth0/address 2>/dev/null || echo unknown)
echo "IVC-STARRY-NET iface=eth0 mac=$mac ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1"

# AxVisor notifies the StarryOS VM before the Zephyr VM. Give the peer enough
# time to bind its UDP socket so the first measured command is not a startup
# retransmission. This delay precedes raw-sample collection in every profile.
peer_startup_delay_seconds=2
echo "IVC-STARRY-PEER-WAIT seconds=2"
"$BB" sleep "$peer_startup_delay_seconds"

if [ "${ivc_fault_profile:-none}" = restart ] && [ ! -r "$restart_marker" ]; then
    if /usr/local/bin/ivcproto controller \
        10.0.0.2:5500 "$ivc_restart_first_count" "$ivc_mode" "$ivc_period_ms" \
        "$ivc_restart_previous_session" --backend "$ivc_backend" \
        --raw-csv "$restart_raw_csv" --fault-profile none \
        --ack-timeout-ms "$ivc_restart_ack_timeout_ms" \
        >"$restart_controller_log" 2>&1; then
        validate_raw_csv "$restart_raw_csv" "$ivc_restart_first_count"
    else
        fatal restart-first-controller-failed
    fi
    restart_raw_sha256=$validated_raw_sha256
    restart_uart_sha256=$(printf '%s' "$restart_raw_sha256" | "$BB" cut -c1-12)
    log_checksum=$("$BB" sha256sum "$restart_controller_log") \
        || fatal restart-log-hash-failed
    restart_log_sha256=${log_checksum%% *}
    "$BB" printf '%s\n' \
        'schema=1' \
        "previous_session=$ivc_restart_previous_session" \
        "samples=$ivc_restart_first_count" \
        "raw_sha256=$restart_raw_sha256" \
        "log_sha256=$restart_log_sha256" \
        >"$restart_marker.new" || fatal restart-marker-write-failed
    "$BB" mv -f "$restart_marker.new" "$restart_marker" \
        || fatal restart-marker-rename-failed
    "$BB" sync || fatal restart-phase-sync-failed
    copy=0
    while [ "$copy" -lt 3 ]; do
        echo "IVC-STARRY-RESTART-ARMED phase=before-reset session_id=$ivc_restart_previous_session samples=$ivc_restart_first_count"
        echo "IVC-STARRY-RESTART-RAW path=$restart_raw_csv samples=$ivc_restart_first_count sha256=$restart_uart_sha256"
        copy=$((copy + 1))
        "$BB" sleep 1
    done
    while true; do
        "$BB" sleep 60
    done
fi

if [ "${ivc_fault_profile:-none}" = restart ]; then
    [ -r "$restart_marker" ] || fatal restart-marker-not-found
    stored_schema=$("$BB" sed -n 's/^schema=//p' "$restart_marker")
    stored_session=$("$BB" sed -n 's/^previous_session=//p' "$restart_marker")
    stored_samples=$("$BB" sed -n 's/^samples=//p' "$restart_marker")
    stored_raw_sha256=$("$BB" sed -n 's/^raw_sha256=//p' "$restart_marker")
    [ "$stored_schema" = 1 ] || fatal restart-marker-schema-mismatch
    [ "$stored_session" = "$ivc_restart_previous_session" ] \
        || fatal restart-marker-session-mismatch
    [ "$stored_samples" = "$ivc_restart_first_count" ] \
        || fatal restart-marker-count-mismatch
    validate_raw_csv "$restart_raw_csv" "$ivc_restart_first_count"
    [ "$validated_raw_sha256" = "$stored_raw_sha256" ] \
        || fatal restart-marker-raw-hash-mismatch
    restart_resume_copy=0
    while [ "$restart_resume_copy" -lt 3 ]; do
        echo "IVC-STARRY-RESTART-RESUME phase=after-reset old_session=$ivc_restart_previous_session new_session=$ivc_restart_current_session first_samples=$ivc_restart_first_count"
        restart_resume_copy=$((restart_resume_copy + 1))
        "$BB" sleep 0.1
    done
    if /usr/local/bin/ivcproto controller \
        10.0.0.2:5500 "$ivc_count" "$ivc_mode" "$ivc_period_ms" \
        "$ivc_restart_current_session" --backend "$ivc_backend" \
        --raw-csv "$ivc_raw_csv" --fault-profile restart \
        --restart-previous-session "$ivc_restart_previous_session" \
        --ack-timeout-ms "$ivc_restart_ack_timeout_ms"; then
        result=0
    else
        result=$?
    fi
elif [ "$ivc_backend" = rknn-npu ]; then
    if /usr/local/bin/ivcproto controller \
        10.0.0.2:5500 "$ivc_count" "$ivc_mode" "$ivc_period_ms" \
        --backend "$ivc_backend" --raw-csv "$ivc_raw_csv" \
        --fault-profile none --ack-timeout-ms "$ivc_ack_timeout_ms" \
        --rknn-model "$ivc_rknn_model" --rknn-evidence "$ivc_rknn_evidence"; then
        result=0
    else
        result=$?
    fi
elif /usr/local/bin/ivcproto controller \
    10.0.0.2:5500 "$ivc_count" "$ivc_mode" "$ivc_period_ms" \
    --backend "$ivc_backend" --raw-csv "$ivc_raw_csv" \
    --fault-profile "${ivc_fault_profile:-none}" \
    --ack-timeout-ms "$ivc_ack_timeout_ms"; then
    result=0
else
    result=$?
fi

if [ "$result" -eq 0 ]; then
    validate_raw_csv "$ivc_raw_csv" "$ivc_count"
    raw_sha256=$validated_raw_sha256
    if [ "$ivc_backend" = rknn-npu ]; then
        validate_rknn_evidence
    fi
    # Let the peer VM shutdown messages drain before publishing redundant
    # identities on the shared UART.
    raw_identity_quiet_seconds=4
    raw_identity_line_interval_seconds=0.25
    raw_identity_copy_interval_seconds=1
    "$BB" sleep "$raw_identity_quiet_seconds"
    raw_identity_copy=0
    while [ "$raw_identity_copy" -lt 5 ]; do
        echo "IVC-STARRY-RAW path=$ivc_raw_csv samples=$ivc_count sha256=$raw_sha256"
        if [ "$ivc_backend" = rknn-npu ]; then
            "$BB" sleep "$raw_identity_line_interval_seconds"
            echo "IVC-STARRY-RKNN-MODEL sha256=$actual_rknn_model_sha256"
            "$BB" sleep "$raw_identity_line_interval_seconds"
            # The guest-console routing prefix shares a fixed line budget with
            # this marker, so keep the full digest record compact.
            echo "IVC-STARRY-RKNN-RAW sha256=$validated_rknn_sha256"
            "$BB" sleep "$raw_identity_line_interval_seconds"
        fi
        raw_identity_copy=$((raw_identity_copy + 1))
        "$BB" sleep "$raw_identity_copy_interval_seconds"
    done
    result=0
fi
"$BB" sync || fatal final-sync-failed
echo "IVC-STARRY-DONE exit=$result"
"$BB" poweroff -f
"$BB" sleep 5
fatal poweroff-returned
