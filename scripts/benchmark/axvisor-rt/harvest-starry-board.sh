#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
analyzer=$script_dir/analyze_starry_board.py
irq_analyzer=$script_dir/analyze_irq_trace.py
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
result_image=${ORANGEPI_RT_RESULT_IMAGE:?set ORANGEPI_RT_RESULT_IMAGE to the snapshotted StarryOS rootfs image}
raw_output=${ORANGEPI_RT_RAW_LOG:?set ORANGEPI_RT_RAW_LOG to an absolute local raw-log path}
summary_output=${ORANGEPI_RT_SUMMARY_JSON:?set ORANGEPI_RT_SUMMARY_JSON to an absolute local summary path}
guest_irq_output=${ORANGEPI_RT_GUEST_IRQ_LOG:-${raw_output}.guest-irq.log.gz}
host_trace_output=${ORANGEPI_RT_HOST_TRACE_LOG:-${raw_output}.host.log}
profile=${ORANGEPI_RT_PROFILE:?set ORANGEPI_RT_PROFILE to shared or partitioned}
expected_workload=${ORANGEPI_RT_EXPECTED_WORKLOAD:-idle}
expected_iterations=${ORANGEPI_RT_EXPECTED_ITERATIONS:-100}
expected_host_noise_pcpu=${ORANGEPI_RT_EXPECTED_HOST_NOISE_PCPU:-}
guest_raw_path=/var/lib/axvisor-rt/raw.log
guest_irq_path=/var/lib/axvisor-rt/guest-timer-trace.log.gz
host_trace_remote=${result_image}.host.log

case "$result_image" in
    /home/orangepi/*|/home/rt) ;;
    *)
        echo "ORANGEPI_RT_RESULT_IMAGE is outside the approved /home paths: $result_image" >&2
        exit 1
        ;;
esac
if [[ "$result_image" =~ [^A-Za-z0-9_./-] ]]; then
    echo "ORANGEPI_RT_RESULT_IMAGE contains unsupported characters: $result_image" >&2
    exit 1
fi
for output in "$raw_output" "$summary_output" "$guest_irq_output" "$host_trace_output"; do
    if [[ "$output" != /* ]]; then
        echo "StarryOS RT harvest outputs must use absolute paths: $output" >&2
        exit 1
    fi
done
if [[ $(printf '%s\n' \
    "$raw_output" "$summary_output" "$guest_irq_output" "$host_trace_output" | sort -u | wc -l) -ne 4 ]]; then
    echo "raw, summary, guest IRQ, and host trace outputs must be different paths" >&2
    exit 1
fi
case "$profile" in
    shared|partitioned) ;;
    *) echo "ORANGEPI_RT_PROFILE must be shared or partitioned" >&2; exit 2 ;;
esac
case "$expected_workload" in
    idle|cpu-stress) ;;
    *) echo "ORANGEPI_RT_EXPECTED_WORKLOAD must be idle or cpu-stress" >&2; exit 2 ;;
esac
case "$expected_iterations" in
    ''|*[!0-9]*)
        echo "ORANGEPI_RT_EXPECTED_ITERATIONS must be a positive integer" >&2
        exit 2
        ;;
esac
if ((expected_iterations == 0)); then
    echo "ORANGEPI_RT_EXPECTED_ITERATIONS must be a positive integer" >&2
    exit 2
fi
case "$expected_host_noise_pcpu" in
    ''|*[!0-9]*)
        if [[ -n "$expected_host_noise_pcpu" ]]; then
            echo "ORANGEPI_RT_EXPECTED_HOST_NOISE_PCPU must be a non-negative integer" >&2
            exit 2
        fi
        ;;
esac
if [[ ! -r "$ssh_identity" || ! -r "$analyzer" || ! -r "$irq_analyzer" ]]; then
    echo "harvest identity or analyzer is not readable" >&2
    exit 1
fi
for command_name in cargo grep mkdir mktemp mv python3 sha256sum sort ssh wc; do
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "required StarryOS RT harvest tool not found: $command_name" >&2
        exit 1
    }
done

lease_dir=$workspace/tmp/axvisor-rt/board-lease
mkdir -p \
    "$lease_dir" \
    "$(dirname -- "$raw_output")" \
    "$(dirname -- "$summary_output")" \
    "$(dirname -- "$guest_irq_output")" \
    "$(dirname -- "$host_trace_output")"
lease_log=$(mktemp "$lease_dir/connect.XXXXXX.log")
temporary_raw=$(mktemp "$(dirname -- "$raw_output")/.starry-raw.XXXXXX.log")
temporary_summary=$(mktemp "$(dirname -- "$summary_output")/.starry-summary.XXXXXX.json")
temporary_guest_irq=$(mktemp "$(dirname -- "$guest_irq_output")/.starry-guest-irq.XXXXXX.log.gz")
temporary_host_trace=$(mktemp "$(dirname -- "$host_trace_output")/.starry-host-trace.XXXXXX.log")
remote_status=$(mktemp "$lease_dir/harvest.XXXXXX.log")
lease_pid=
cleanup() {
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    rm -f -- \
        "$lease_log" "$temporary_raw" "$temporary_summary" \
        "$temporary_guest_irq" "$temporary_host_trace" "$remote_status"
}
trap cleanup EXIT HUP INT TERM

cd "$workspace"
cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$lease_log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "board lease ended before StarryOS RT harvest allocation" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "timed out acquiring $board_type for StarryOS RT harvest" >&2
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
    "$result_image" "$guest_raw_path" "$guest_irq_path" \
    >"$temporary_raw" 2>"$remote_status" <<'REMOTE'
set -eu
result_image=$1
guest_raw_path=$2
guest_irq_path=$3
temporary=$(mktemp /tmp/starry-rt-raw.XXXXXX.log)
temporary_guest_irq=$(mktemp /tmp/starry-rt-guest-irq.XXXXXX.log.gz)
fsck_log=$(mktemp /tmp/starry-rt-fsck.XXXXXX.log)
debugfs_log=$(mktemp /tmp/starry-rt-debugfs.XXXXXX.log)
repaired_image=$(mktemp /tmp/starry-rt-repaired.XXXXXX.img)
repaired_raw=$(mktemp /tmp/starry-rt-repaired-raw.XXXXXX.log)
repaired_guest_irq=$(mktemp /tmp/starry-rt-repaired-guest-irq.XXXXXX.log.gz)
repair_log=$(mktemp /tmp/starry-rt-repair.XXXXXX.log)
verify_log=$(mktemp /tmp/starry-rt-verify.XXXXXX.log)
cleanup() {
    rm -f -- \
        "$temporary" "$temporary_guest_irq" "$fsck_log" "$debugfs_log" \
        "$repaired_image" "$repaired_raw" "$repaired_guest_irq" \
        "$repair_log" "$verify_log"
}
trap cleanup EXIT HUP INT TERM

sync
e2fsck_path=$(command -v e2fsck)
debugfs_path=$(command -v debugfs)
if ! "$debugfs_path" -R "dump $guest_raw_path $temporary" \
    "$result_image" >"$debugfs_log" 2>&1; then
    echo "debugfs could not extract $guest_raw_path from $result_image" >&2
    cat "$debugfs_log" >&2
    exit 1
fi
if [ ! -s "$temporary" ]; then
    echo "debugfs extracted an empty $guest_raw_path from $result_image" >&2
    cat "$debugfs_log" >&2
    exit 1
fi
if ! "$debugfs_path" -R "dump $guest_irq_path $temporary_guest_irq" \
    "$result_image" >>"$debugfs_log" 2>&1; then
    echo "debugfs could not extract $guest_irq_path from $result_image" >&2
    cat "$debugfs_log" >&2
    exit 1
fi
if [ ! -s "$temporary_guest_irq" ]; then
    echo "debugfs extracted an empty $guest_irq_path from $result_image" >&2
    exit 1
fi
set +e
"$e2fsck_path" -fn "$result_image" >"$fsck_log" 2>&1
fsck_status=$?
set -e
case "$fsck_status" in
    0)
        filesystem_state=clean
        echo "AXVISOR_RT_SNAPSHOT_FSCK state=$filesystem_state original_status=0" >&2
        ;;
    4)
        # Never repair the authoritative snapshot. Repair a disposable copy,
        # then require its raw log to be byte-identical to the direct extract.
        cp --reflink=auto "$result_image" "$repaired_image"
        set +e
        "$e2fsck_path" -fy "$repaired_image" >"$repair_log" 2>&1
        repair_status=$?
        set -e
        case "$repair_status" in
            0|1) ;;
            *)
                echo "copy-only filesystem repair failed with status $repair_status" >&2
                cat "$fsck_log" >&2
                cat "$repair_log" >&2
                exit 1
                ;;
        esac
        if ! "$e2fsck_path" -fn "$repaired_image" >"$verify_log" 2>&1; then
            echo "repaired snapshot copy did not pass read-only verification" >&2
            cat "$fsck_log" >&2
            cat "$repair_log" >&2
            cat "$verify_log" >&2
            exit 1
        fi
        if ! "$debugfs_path" -R "dump $guest_raw_path $repaired_raw" \
            "$repaired_image" >>"$debugfs_log" 2>&1; then
            echo "debugfs could not extract raw data from the repaired copy" >&2
            cat "$debugfs_log" >&2
            exit 1
        fi
        if ! "$debugfs_path" -R "dump $guest_irq_path $repaired_guest_irq" \
            "$repaired_image" >>"$debugfs_log" 2>&1; then
            echo "debugfs could not extract guest IRQ trace from the repaired copy" >&2
            cat "$debugfs_log" >&2
            exit 1
        fi
        if ! cmp -s "$temporary" "$repaired_raw"; then
            echo "direct and repaired-copy raw logs differ" >&2
            exit 1
        fi
        if ! cmp -s "$temporary_guest_irq" "$repaired_guest_irq"; then
            echo "direct and repaired-copy guest IRQ traces differ" >&2
            exit 1
        fi
        filesystem_state=unclean-orphans-raw-stable-after-copy-repair
        cat "$fsck_log" >&2
        echo "AXVISOR_RT_SNAPSHOT_FSCK state=$filesystem_state original_status=4 repair_status=$repair_status repaired_verify_status=0" >&2
        ;;
    *)
        echo "read-only filesystem check failed with unsupported status $fsck_status" >&2
        cat "$fsck_log" >&2
        exit 1
        ;;
esac
guest_irq_bytes=$(wc -c <"$temporary_guest_irq")
guest_irq_sha256=$(sha256sum "$temporary_guest_irq")
guest_irq_sha256=${guest_irq_sha256%% *}
echo "AXVISOR_RT_GUEST_IRQ_EXTRACT bytes=$guest_irq_bytes sha256=$guest_irq_sha256" >&2
cat "$temporary"
REMOTE

cat "$remote_status" >&2
filesystem_state=$(
    sed -n 's/^AXVISOR_RT_SNAPSHOT_FSCK state=\([^ ]*\).*$/\1/p' "$remote_status"
)
if [[ -z "$filesystem_state" || "$filesystem_state" == *$'\n'* ]]; then
    echo "remote harvest did not report exactly one snapshot filesystem state" >&2
    exit 1
fi

ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$result_image" "$guest_irq_path" >"$temporary_guest_irq" <<'REMOTE'
set -eu
result_image=$1
guest_irq_path=$2
temporary=$(mktemp /tmp/starry-rt-guest-irq-fetch.XXXXXX.log.gz)
trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
debugfs_path=$(command -v debugfs)
"$debugfs_path" -R "dump $guest_irq_path $temporary" "$result_image" >/dev/null 2>&1
test -s "$temporary"
cat "$temporary"
REMOTE

ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$host_trace_remote" >"$temporary_host_trace" <<'REMOTE'
set -eu
host_trace=$1
case "$host_trace" in
    /home/orangepi/*|/home/rt.host.log) ;;
    *) echo "host trace path is outside the approved /home paths: $host_trace" >&2; exit 1 ;;
esac
test -s "$host_trace"
cat "$host_trace"
REMOTE

expected_guest_irq_identity=$(
    sed -n 's/^AXVISOR_RT_GUEST_IRQ_EXTRACT bytes=\([0-9][0-9]*\) sha256=\([0-9a-f][0-9a-f]*\)$/\1:\2/p' \
        "$remote_status"
)
guest_irq_bytes=$(wc -c <"$temporary_guest_irq")
guest_irq_sha256=$(sha256sum "$temporary_guest_irq")
guest_irq_sha256=${guest_irq_sha256%% *}
if [[ "$expected_guest_irq_identity" != "$guest_irq_bytes:$guest_irq_sha256" ]]; then
    echo "second guest IRQ extraction differs from the fsck-validated extraction" >&2
    exit 1
fi
if [[ ! -s "$temporary_host_trace" ]]; then
    echo "AxVisor host RT trace is empty" >&2
    exit 1
fi

analyzer_args=(
    "$temporary_raw"
    --profile "$profile"
    --expected-workload "$expected_workload"
    --expected-iterations "$expected_iterations"
    --evidence-path "$raw_output"
    --filesystem-state "$filesystem_state"
    --host-trace "$temporary_host_trace"
    --guest-irq-trace "$temporary_guest_irq"
    --host-trace-evidence-path "$host_trace_output"
    --guest-irq-trace-evidence-path "$guest_irq_output"
    --output "$temporary_summary"
)
if [[ -n "$expected_host_noise_pcpu" ]]; then
    analyzer_args+=(--expected-host-noise-pcpu "$expected_host_noise_pcpu")
fi
python3 "$analyzer" "${analyzer_args[@]}"

raw_sha256=$(sha256sum "$temporary_raw")
raw_sha256=${raw_sha256%% *}
raw_lines=$(wc -l <"$temporary_raw")
host_trace_sha256=$(sha256sum "$temporary_host_trace")
host_trace_sha256=${host_trace_sha256%% *}
result_evidence=$(ssh "${ssh_options[@]}" "$ssh_target" sh -s -- "$result_image" <<'REMOTE'
set -eu
result_image=$1
test -s "$result_image"
image_bytes=$(wc -c <"$result_image")
image_sha256=$(sha256sum "$result_image")
image_sha256=${image_sha256%% *}
case "$image_bytes:$image_sha256" in
    *[!0-9a-f:]*|:*) echo "snapshot identity is invalid" >&2; exit 1 ;;
esac
if [ "${#image_sha256}" -ne 64 ]; then
    echo "snapshot SHA-256 length is invalid" >&2
    exit 1
fi
printf 'AXVISOR_RT_SNAPSHOT_IDENTITY path=%s bytes=%s sha256=%s\n' \
    "$result_image" "$image_bytes" "$image_sha256"
REMOTE
)

mv -f -- "$temporary_raw" "$raw_output"
temporary_raw=
mv -f -- "$temporary_summary" "$summary_output"
temporary_summary=
mv -f -- "$temporary_guest_irq" "$guest_irq_output"
temporary_guest_irq=
mv -f -- "$temporary_host_trace" "$host_trace_output"
temporary_host_trace=

kill -TERM "$lease_pid" 2>/dev/null || true
wait "$lease_pid" 2>/dev/null || true
lease_pid=

printf '%s\n' "$result_evidence"
host_noise_pcpu=${expected_host_noise_pcpu:-none}
echo "AXVISOR_RT_STARRY_HARVESTED profile=$profile workload=$expected_workload host_noise_pcpu=$host_noise_pcpu samples_per_metric=$expected_iterations lines=$raw_lines sha256=$raw_sha256 guest_irq_sha256=$guest_irq_sha256 host_trace_sha256=$host_trace_sha256 filesystem_state=$filesystem_state raw=$raw_output guest_irq=$guest_irq_output host_trace=$host_trace_output summary=$summary_output"
