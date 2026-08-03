#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
result_image=${ORANGEPI_IVC_RESULT_IMAGE:?set ORANGEPI_IVC_RESULT_IMAGE to the snapshotted StarryOS rootfs image}
raw_output=${ORANGEPI_IVC_RAW_CSV:?set ORANGEPI_IVC_RAW_CSV to the local result path}
expected_count=${ORANGEPI_IVC_EXPECTED_COUNT:?set ORANGEPI_IVC_EXPECTED_COUNT to the controller sample count}
pre_reset_raw_output=${ORANGEPI_IVC_PRE_RESET_RAW_CSV:-}
expected_pre_reset_count=${ORANGEPI_IVC_EXPECTED_PRE_RESET_COUNT:-0}
guest_raw_path=/var/lib/ivc/raw.csv
guest_raw_manifest_path=/var/lib/ivc/raw.csv.sha256
guest_pre_reset_raw_path=/var/lib/ivc/raw-before-reset.csv
guest_pre_reset_raw_manifest_path=/var/lib/ivc/raw-before-reset.csv.sha256
lease_dir=$workspace/tmp/competition/ivc/board-lease
csv_header='sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,command_actuator_permille,status_actuator_permille,error_milli_c'

case "$result_image" in
    /home/orangepi/*) ;;
    *)
        echo "ORANGEPI_IVC_RESULT_IMAGE must remain below /home/orangepi: $result_image" >&2
        exit 1
        ;;
esac
if [[ "$result_image" =~ [^A-Za-z0-9_./-] ]]; then
    echo "ORANGEPI_IVC_RESULT_IMAGE contains unsupported characters: $result_image" >&2
    exit 1
fi
if [[ "$raw_output" != /* ]]; then
    echo "ORANGEPI_IVC_RAW_CSV must be an absolute path: $raw_output" >&2
    exit 1
fi
case "$expected_count" in
    ''|*[!0-9]*)
        echo "ORANGEPI_IVC_EXPECTED_COUNT must be a positive integer" >&2
        exit 2
        ;;
esac
if ((expected_count == 0)); then
    echo "ORANGEPI_IVC_EXPECTED_COUNT must be a positive integer" >&2
    exit 2
fi
case "$expected_pre_reset_count" in
    ''|*[!0-9]*)
        echo "ORANGEPI_IVC_EXPECTED_PRE_RESET_COUNT must be a nonnegative integer" >&2
        exit 2
        ;;
esac
if ((expected_pre_reset_count > 0)); then
    if [[ -z "$pre_reset_raw_output" || "$pre_reset_raw_output" != /* ]]; then
        echo "ORANGEPI_IVC_PRE_RESET_RAW_CSV must be an absolute path" >&2
        exit 1
    fi
    if [[ "$pre_reset_raw_output" == "$raw_output" ]]; then
        echo "Pre-reset and post-reset raw output paths must differ" >&2
        exit 1
    fi
fi
if [[ ! -r "$ssh_identity" ]]; then
    echo "Orange Pi SSH identity is not readable: $ssh_identity" >&2
    exit 1
fi
for command_name in cargo grep mkdir mktemp mv rsync sha256sum ssh wc; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required Orange Pi harvest tool not found: $command_name" >&2
        exit 1
    fi
done

mkdir -p "$lease_dir" "$(dirname -- "$raw_output")"
lease_log=$(mktemp "$lease_dir/connect.XXXXXX.log")
temporary_output=$(mktemp "$(dirname -- "$raw_output")/.raw.csv.XXXXXX")
pre_reset_temporary_output=
if ((expected_pre_reset_count > 0)); then
    mkdir -p "$(dirname -- "$pre_reset_raw_output")"
    pre_reset_temporary_output=$(mktemp \
        "$(dirname -- "$pre_reset_raw_output")/.raw-before-reset.csv.XXXXXX")
fi
lease_pid=
cleanup() {
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    rm -f -- "$lease_log" "$temporary_output" "$pre_reset_temporary_output"
}
trap cleanup EXIT HUP INT TERM

cd "$workspace"
cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$lease_log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "Board lease ended before result harvest allocation" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "Timed out while acquiring the $board_type lease for result harvest" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    sleep 0.2
done

ssh_options=(
    -i "$ssh_identity"
    -o IdentitiesOnly=yes
    -o BatchMode=yes
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=8
)
ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$result_image" "$guest_raw_path" >"$temporary_output" <<'REMOTE'
set -eu
result_image=$1
guest_raw_path=$2
temporary=$(mktemp /tmp/ivc-raw.XXXXXX.csv)
cleanup() {
    rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

sync
debugfs_path=$(command -v debugfs)
e2fsck_path=$(command -v e2fsck)
"$e2fsck_path" -fn "$result_image" >/dev/null 2>&1
"$debugfs_path" -R "dump $guest_raw_path $temporary" "$result_image" >/dev/null
test -s "$temporary"
cat "$temporary"
REMOTE

IFS= read -r actual_header <"$temporary_output"
if [[ "$actual_header" != "$csv_header" ]]; then
    echo "Harvested controller CSV header does not match the evidence schema" >&2
    exit 1
fi
line_count=$(wc -l <"$temporary_output")
expected_lines=$((expected_count + 1))
if ((line_count != expected_lines)); then
    echo "Harvested controller CSV has $line_count lines; expected $expected_lines" >&2
    exit 1
fi
raw_sha256=$(sha256sum "$temporary_output")
raw_sha256=${raw_sha256%% *}
guest_raw_manifest=$(ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$result_image" "$guest_raw_manifest_path" <<'REMOTE'
set -eu
result_image=$1
guest_raw_manifest_path=$2
temporary=$(mktemp /tmp/ivc-raw-sha256.XXXXXX)
cleanup() {
    rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

debugfs_path=$(command -v debugfs)
"$debugfs_path" -R "dump $guest_raw_manifest_path $temporary" \
    "$result_image" >/dev/null
test -s "$temporary"
cat "$temporary"
REMOTE
)
expected_guest_raw_manifest="$raw_sha256  $guest_raw_path"
if [[ "$guest_raw_manifest" != "$expected_guest_raw_manifest" ]]; then
    echo "Snapshot guest raw manifest does not match the harvested CSV" >&2
    exit 1
fi
mv -f -- "$temporary_output" "$raw_output"
temporary_output=

pre_reset_raw_sha256=
if ((expected_pre_reset_count > 0)); then
    ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
        "$result_image" "$guest_pre_reset_raw_path" \
        >"$pre_reset_temporary_output" <<'REMOTE'
set -eu
result_image=$1
guest_raw_path=$2
temporary=$(mktemp /tmp/ivc-pre-reset-raw.XXXXXX.csv)
cleanup() {
    rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

debugfs_path=$(command -v debugfs)
"$debugfs_path" -R "dump $guest_raw_path $temporary" "$result_image" >/dev/null
test -s "$temporary"
cat "$temporary"
REMOTE

    IFS= read -r pre_reset_header <"$pre_reset_temporary_output"
    if [[ "$pre_reset_header" != "$csv_header" ]]; then
        echo "Harvested pre-reset CSV header does not match the evidence schema" >&2
        exit 1
    fi
    pre_reset_line_count=$(wc -l <"$pre_reset_temporary_output")
    expected_pre_reset_lines=$((expected_pre_reset_count + 1))
    if ((pre_reset_line_count != expected_pre_reset_lines)); then
        echo "Harvested pre-reset CSV has $pre_reset_line_count lines; expected $expected_pre_reset_lines" >&2
        exit 1
    fi
    pre_reset_raw_sha256=$(sha256sum "$pre_reset_temporary_output")
    pre_reset_raw_sha256=${pre_reset_raw_sha256%% *}
    guest_pre_reset_manifest=$(ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
        "$result_image" "$guest_pre_reset_raw_manifest_path" <<'REMOTE'
set -eu
result_image=$1
guest_manifest_path=$2
temporary=$(mktemp /tmp/ivc-pre-reset-sha256.XXXXXX)
cleanup() {
    rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

debugfs_path=$(command -v debugfs)
"$debugfs_path" -R "dump $guest_manifest_path $temporary" "$result_image" >/dev/null
test -s "$temporary"
cat "$temporary"
REMOTE
    )
    expected_pre_reset_manifest="$pre_reset_raw_sha256  $guest_pre_reset_raw_path"
    if [[ "$guest_pre_reset_manifest" != "$expected_pre_reset_manifest" ]]; then
        echo "Snapshot pre-reset raw manifest does not match the harvested CSV" >&2
        exit 1
    fi
    mv -f -- "$pre_reset_temporary_output" "$pre_reset_raw_output"
    pre_reset_temporary_output=
fi

result_evidence=$(ssh "${ssh_options[@]}" "$ssh_target" sh -s -- "$result_image" <<'REMOTE'
set -eu

result_image=$1
test -s "$result_image"
image_bytes=$(wc -c <"$result_image")
image_sha256=$(sha256sum "$result_image")
image_sha256=${image_sha256%% *}
case "$image_bytes" in
    ''|*[!0-9]*)
        echo "Result image size is invalid" >&2
        exit 1
        ;;
esac
case "$image_sha256" in
    ''|*[!0-9a-f]*)
        echo "Result image SHA-256 is invalid" >&2
        exit 1
        ;;
esac
if [ "${#image_sha256}" -ne 64 ]; then
    echo "Result image SHA-256 has an invalid length" >&2
    exit 1
fi
printf 'BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 path=%s bytes=%s sha256=%s fsck=clean\n' \
    "$result_image" "$image_bytes" "$image_sha256"
REMOTE
)

board_identity=$(ssh "${ssh_options[@]}" "$ssh_target" sh -s <<'REMOTE'
set -eu

board_id=
if [ -r /proc/device-tree/serial-number ]; then
    board_id=$(tr -d '\000\r\n ' </proc/device-tree/serial-number)
fi
if [ -z "$board_id" ] && [ -r /etc/machine-id ]; then
    board_id=$(tr -d '\r\n ' </etc/machine-id)
fi
hostname_value=$(hostname | tr -cd 'A-Za-z0-9._-')
cpu_temp_milli_c=
for thermal_zone in /sys/class/thermal/thermal_zone*; do
    [ -r "$thermal_zone/type" ] || continue
    thermal_type=$(cat "$thermal_zone/type")
    case "$thermal_type" in
        *cpu*|*CPU*|*soc*|*SOC*)
            cpu_temp_milli_c=$(cat "$thermal_zone/temp")
            break
            ;;
    esac
done
if [ -z "$cpu_temp_milli_c" ]; then
    for thermal_zone in /sys/class/thermal/thermal_zone*; do
        if [ -r "$thermal_zone/temp" ]; then
            cpu_temp_milli_c=$(cat "$thermal_zone/temp")
            break
        fi
    done
fi
[ -n "$board_id" ] || {
    echo "Board ID is unavailable" >&2
    exit 1
}
[ -n "$hostname_value" ] || {
    echo "Board hostname is unavailable" >&2
    exit 1
}
case "$board_id:$hostname_value:$cpu_temp_milli_c" in
    *[!A-Za-z0-9._:-]*)
        echo "Board identity contains unsupported characters" >&2
        exit 1
        ;;
esac
case "$cpu_temp_milli_c" in
    ''|*[!0-9-]*)
        echo "Board CPU temperature is unavailable" >&2
        exit 1
        ;;
esac
printf 'BOARD_IDENTITY board_id=%s hostname=%s cpu_temp_milli_c=%s\n' \
    "$board_id" "$hostname_value" "$cpu_temp_milli_c"
REMOTE
)

kill -TERM "$lease_pid" 2>/dev/null || true
wait "$lease_pid" 2>/dev/null || true
lease_pid=

printf '%s\n' "$result_evidence"
echo "BOARD_GUEST_RAW_MANIFEST path=$guest_raw_path samples=$expected_count sha256=$raw_sha256"
echo "BOARD_RAW_RESULT_HARVESTED path=$raw_output samples=$expected_count sha256=$raw_sha256"
if ((expected_pre_reset_count > 0)); then
    echo "BOARD_GUEST_PRE_RESET_RAW_MANIFEST path=$guest_pre_reset_raw_path samples=$expected_pre_reset_count sha256=$pre_reset_raw_sha256"
    echo "BOARD_PRE_RESET_RAW_RESULT_HARVESTED path=$pre_reset_raw_output samples=$expected_pre_reset_count sha256=$pre_reset_raw_sha256"
fi
printf '%s\n' "$board_identity"
