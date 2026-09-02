#!/usr/bin/env bash

set -euo pipefail

formal_sudo_password=${ORANGEPI_SUDO_PASSWORD-}
unset ORANGEPI_SUDO_PASSWORD

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
contract=$script_dir/formal_campaign.py
kernel_builder=$script_dir/build-starry-kernel.sh
host_toolchain_preparer=$script_dir/prepare-freestanding-c-toolchain.sh
rootfs_builder=$script_dir/build-starry-rootfs.sh
probe_builder=$script_dir/build-probe.sh
soak_builder=$script_dir/prepare-starry-soak.sh
dtb_builder=$script_dir/board/build-guest-dtb.sh
stage_runner=$script_dir/stage-starry-board.sh
board_runner=$script_dir/board/board-runner.sh
harvest_runner=$script_dir/harvest-starry-board.sh
pair_comparator=$script_dir/compare_starry_board.py
campaign_aggregator=$script_dir/aggregate_starry_board.py

pair_kernel=$workspace/tmp/axvisor-rt/starryos-rt.bin
pair_rootfs=$workspace/tmp/axvisor-rt/starry-rt-capture-rootfs.img
soak_kernel=$workspace/tmp/axvisor-rt/starryos-rt-soak.bin
soak_rootfs=$workspace/tmp/axvisor-rt/starry-rt-soak-rootfs.img
guest_dtb=$workspace/tmp/axvisor-rt/guest-dtb/starry-orangepi-5-plus.dtb
probe=$workspace/tmp/axvisor-rt/axvisor-rt-probe

usage() {
    cat <<'EOF'
usage: run-formal-campaign.sh COMMAND --result-dir PATH [options]

Commands:
  prepare     Build pair/soak inputs and write the immutable preregistration.
  status      Print machine-readable completion and next-slot state.
  run-next    Run exactly the next frozen pair or soak slot.
  run-all     Resume until all five pairs and both soak slots complete.
  aggregate   Rebuild pair comparisons and the final M2 campaign summary.

Prepare options:
  --expected-commit SHA  Full clean source commit to bind.
  --source-ref REF       Human-readable branch or release reference.
  --base-rootfs PATH     Frozen AArch64 BusyBox base ext4 image.
  --hardware-id ID       Physical serial-number/machine-id.
  --hostname NAME        Expected Linux hostname.
  --service-id ID        Board-service ID (default: orangepi-5-plus-1).
  --board TYPE           Board type (default: OrangePi-5-Plus).
  --pair-timeout SEC     Per pair-half timeout (default: 900).
  --soak-timeout SEC     Per soak-half timeout (default: 4500).

The result root must not already exist for prepare. If it is inside the Git
worktree, Git must ignore it. Failed attempts remain under attempts/ and never
create receipt.json; completed evidence is immutable and run-all is resumable.
EOF
}

fail() {
    echo "AXVISOR_RT_FORMAL_CAMPAIGN_FAILED: $*" >&2
    exit 1
}

resolve_path() {
    local path=$1

    if [[ "$path" == /* ]]; then
        realpath -m -- "$path"
    else
        realpath -m -- "$workspace/$path"
    fi
}

require_positive_integer() {
    local name=$1
    local value=$2

    if [[ ! "$value" =~ ^[0-9]+$ ]] || ((value == 0)); then
        fail "$name must be a positive integer"
    fi
}

read_record_path() {
    local preregistration=$1
    local name=$2
    local path

    path=$(jq -er --arg name "$name" '.artifacts[$name].path' "$preregistration") || \
        fail "preregistration does not identify artifact $name"
    resolve_path "$path"
}

slot_dir() {
    local result_root=$1
    local phase=$2
    local pair=$3
    local profile=$4

    if [[ "$phase" == pair ]]; then
        printf '%s/pair-%s/%s\n' "$result_root" "$pair" "$profile"
    else
        printf '%s/soak/%s\n' "$result_root" "$profile"
    fi
}

receipt_summary_path() {
    local result_root=$1
    local receipt=$2
    local relative

    relative=$(jq -er '.evidence.summary.path' "$receipt") || \
        fail "receipt does not identify a summary: $receipt"
    resolve_path "$result_root/$relative"
}

write_idempotent_output() {
    local temporary=$1
    local output=$2

    if [[ -e "$output" ]]; then
        cmp -s -- "$temporary" "$output" || \
            fail "existing derived output differs: $output"
        rm -f -- "$temporary"
    else
        mv -- "$temporary" "$output"
    fi
}

action=${1:-}
case "$action" in
    -h|--help)
        usage
        exit 0
        ;;
    "")
        usage >&2
        exit 2
        ;;
    -*)
        echo "unknown formal campaign command: $action" >&2
        usage >&2
        exit 2
        ;;
esac
shift

result_root=
expected_commit=
source_ref=
base_rootfs=
hardware_id=
hostname_value=
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
service_id=orangepi-5-plus-1
pair_timeout=900
soak_timeout=4500

while (($# > 0)); do
    case "$1" in
        --result-dir) result_root=${2:?--result-dir requires a value}; shift 2 ;;
        --expected-commit) expected_commit=${2:?--expected-commit requires a value}; shift 2 ;;
        --source-ref) source_ref=${2:?--source-ref requires a value}; shift 2 ;;
        --base-rootfs) base_rootfs=${2:?--base-rootfs requires a value}; shift 2 ;;
        --hardware-id) hardware_id=${2:?--hardware-id requires a value}; shift 2 ;;
        --hostname) hostname_value=${2:?--hostname requires a value}; shift 2 ;;
        --board) board_type=${2:?--board requires a value}; shift 2 ;;
        --service-id) service_id=${2:?--service-id requires a value}; shift 2 ;;
        --pair-timeout) pair_timeout=${2:?--pair-timeout requires a value}; shift 2 ;;
        --soak-timeout) soak_timeout=${2:?--soak-timeout requires a value}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown formal campaign option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[[ -n "$result_root" ]] || {
    echo "--result-dir is required" >&2
    exit 2
}
result_root=$(resolve_path "$result_root")
preregistration=$result_root/preregistration.json

for command_name in cmp date git jq mktemp mv python3 realpath sha256sum tee; do
    command -v "$command_name" >/dev/null 2>&1 || \
        fail "required command is missing: $command_name"
done
for input_path in \
    "$contract" "$kernel_builder" "$rootfs_builder" "$probe_builder" "$soak_builder" \
    "$host_toolchain_preparer" \
    "$dtb_builder" "$stage_runner" "$board_runner" "$harvest_runner" \
    "$pair_comparator" "$campaign_aggregator"; do
    [[ -r "$input_path" ]] || fail "required campaign input is unreadable: $input_path"
done
if [[ "$action" != prepare ]]; then
    [[ -r "$preregistration" ]] || fail "preregistration is missing"
    [[ -r "$result_root/preregistration.sha256" ]] || \
        fail "preregistration checksum is missing"
    (
        cd "$result_root"
        sha256sum -c preregistration.sha256
    )
fi

case "$action" in
    prepare)
        [[ "$expected_commit" =~ ^[0-9a-f]{40}$ ]] || \
            fail "prepare requires --expected-commit with a full lowercase Git SHA"
        [[ -n "$source_ref" ]] || fail "prepare requires --source-ref"
        [[ -n "$base_rootfs" ]] || fail "prepare requires --base-rootfs"
        [[ -n "$hardware_id" ]] || fail "prepare requires --hardware-id"
        [[ -n "$hostname_value" ]] || fail "prepare requires --hostname"
        require_positive_integer --pair-timeout "$pair_timeout"
        require_positive_integer --soak-timeout "$soak_timeout"
        base_rootfs=$(resolve_path "$base_rootfs")
        [[ -s "$base_rootfs" ]] || fail "base rootfs is missing or empty: $base_rootfs"
        [[ ! -e "$result_root" ]] || fail "refusing to reuse result root: $result_root"
        [[ $(git -C "$workspace" rev-parse HEAD) == "$expected_commit" ]] || \
            fail "workspace HEAD differs from --expected-commit"
        [[ -z $(git -C "$workspace" status --porcelain=v1) ]] || \
            fail "prepare requires a clean Git worktree"
        if [[ "$result_root" == "$workspace"/* ]]; then
            git -C "$workspace" check-ignore -q -- "$result_root/build/pair-kernel.log" || \
                fail "result root inside the worktree must be ignored by Git"
        fi
        mkdir -p "$result_root/build"
        export STARRY_RT_HOST_TOOLCHAIN_MANIFEST=$result_root/build/host-toolchain.json

        bash "$probe_builder" 2>&1 | tee "$result_root/build/probe.log"
        bash "$kernel_builder" 2>&1 | tee "$result_root/build/pair-kernel.log"
        bash "$rootfs_builder" \
            --base-rootfs "$base_rootfs" \
            --mode capture \
            --workload idle \
            --iterations 10000 \
            --warmup 100 \
            --period-us 1000 \
            --measurement-cpu 0 \
            --stress-cpu 1 \
            --fifo-priority 80 \
            --output "$pair_rootfs" \
            2>&1 | tee "$result_root/build/pair-rootfs.log"
        STARRY_RT_BASE_ROOTFS="$base_rootfs" \
            bash "$soak_builder" 2>&1 | tee "$result_root/build/soak.log"
        bash "$dtb_builder" 2>&1 | tee "$result_root/build/guest-dtb.log"

        python3 "$contract" preregister \
            --workspace "$workspace" \
            --expected-commit "$expected_commit" \
            --source-ref "$source_ref" \
            --board-type "$board_type" \
            --service-id "$service_id" \
            --hardware-id "$hardware_id" \
            --hostname "$hostname_value" \
            --base-rootfs "$base_rootfs" \
            --host-toolchain "$result_root/build/host-toolchain.json" \
            --probe "$probe" \
            --pair-kernel "$pair_kernel" \
            --pair-rootfs "$pair_rootfs" \
            --soak-kernel "$soak_kernel" \
            --soak-rootfs "$soak_rootfs" \
            --guest-dtb "$guest_dtb" \
            --pair-timeout "$pair_timeout" \
            --soak-timeout "$soak_timeout" \
            --output "$preregistration"
        (
            cd "$result_root"
            sha256sum preregistration.json >preregistration.sha256
        )
        python3 "$contract" status \
            --workspace "$workspace" \
            --preregistration "$preregistration" \
            --result-root "$result_root"
        ;;
    status)
        [[ -r "$preregistration" ]] || fail "preregistration is missing"
        python3 "$contract" status \
            --workspace "$workspace" \
            --preregistration "$preregistration" \
            --result-root "$result_root"
        ;;
    run-next)
        [[ -r "$preregistration" ]] || fail "preregistration is missing"
        python3 "$contract" verify \
            --workspace "$workspace" \
            --preregistration "$preregistration"
        status_json=$(python3 "$contract" status \
            --workspace "$workspace" \
            --preregistration "$preregistration" \
            --result-root "$result_root")
        if [[ $(jq -r '.next == null' <<<"$status_json") == true ]]; then
            echo "AXVISOR_RT_FORMAL_ALL_SLOTS_COMPLETE result_root=$result_root"
            exit 0
        fi
        phase=$(jq -er '.next.phase' <<<"$status_json")
        profile=$(jq -er '.next.profile' <<<"$status_json")
        pair=$(jq -r '.next.pair // ""' <<<"$status_json")
        slot_root=$(slot_dir "$result_root" "$phase" "$pair" "$profile")
        attempt_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
        attempt_dir=$slot_root/attempts/$attempt_id
        mkdir -p "$attempt_dir"
        stage_log=$attempt_dir/stage.log
        console_log=$attempt_dir/console.log
        harvest_log=$attempt_dir/harvest.log
        raw=$attempt_dir/raw.log
        summary=$attempt_dir/summary.json
        guest_irq=$attempt_dir/guest-irq.log.gz
        host_trace=$attempt_dir/host.log
        started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        completed=0
        record_attempt_status() {
            local exit_status=$?
            if ((completed == 0)); then
                printf 'AXVISOR_RT_FORMAL_ATTEMPT_INCOMPLETE phase=%s pair=%s profile=%s exit=%s\n' \
                "$phase" "${pair:-none}" "$profile" "$exit_status" \
                    >"$attempt_dir/attempt-status.log"
            fi
        }
        trap record_attempt_status EXIT
        trap 'exit 130' HUP INT TERM

        if [[ "$phase" == pair ]]; then
            kernel=$(read_record_path "$preregistration" pair_kernel)
            rootfs=$(read_record_path "$preregistration" pair_rootfs)
            rootfs_name=starry-rt-capture-rootfs.img
            config_kind=formal
            timeout_seconds=$(jq -er '.timeouts_seconds.pair' "$preregistration")
            soak=0
        else
            kernel=$(read_record_path "$preregistration" soak_kernel)
            rootfs=$(read_record_path "$preregistration" soak_rootfs)
            rootfs_name=starry-rt-soak-rootfs.img
            config_kind=soak
            timeout_seconds=$(jq -er '.timeouts_seconds.soak' "$preregistration")
            soak=1
        fi
        guest_dtb_path=$(read_record_path "$preregistration" guest_dtb)
        expected_pcpu=$(jq -er ".measurement.$phase.host_noise.${profile}_pcpu" "$preregistration")
        build_config="scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-$config_kind-$profile.toml"
        board_config="scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-$config_kind-$profile.toml"
        board_type=$(jq -er '.board.type' "$preregistration")

        ORANGEPI_SUDO_PASSWORD="${formal_sudo_password:-orangepi}" \
        ORANGEPI_BOARD_TYPE="$board_type" \
        ORANGEPI_RT_RESULT_IMAGE=/home/rt \
            bash "$stage_runner" \
            --kernel "$kernel" \
            --dtb "$guest_dtb_path" \
            --rootfs "$rootfs" \
            --rootfs-name "$rootfs_name" \
            2>&1 | tee "$stage_log"
        python3 "$contract" validate-stage \
            --workspace "$workspace" \
            --preregistration "$preregistration" \
            --stage-log "$stage_log"

        ORANGEPI_BOARD_TYPE="$board_type" \
        ORANGEPI_AXVISOR_BUILD_CONFIG="$build_config" \
        ORANGEPI_AXVISOR_BOARD_CONFIG="$board_config" \
        ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
        ORANGEPI_RESTORE_LINUX=1 \
        ORANGEPI_RUN_TIMEOUT_SECONDS="$timeout_seconds" \
            bash "$board_runner" 2>&1 | tee "$console_log"

        ORANGEPI_BOARD_TYPE="$board_type" \
        ORANGEPI_RT_RESULT_IMAGE=/home/rt \
        ORANGEPI_RT_RAW_LOG="$raw" \
        ORANGEPI_RT_SUMMARY_JSON="$summary" \
        ORANGEPI_RT_GUEST_IRQ_LOG="$guest_irq" \
        ORANGEPI_RT_HOST_TRACE_LOG="$host_trace" \
        ORANGEPI_RT_PROFILE="$profile" \
        ORANGEPI_RT_EXPECTED_WORKLOAD=idle \
        ORANGEPI_RT_EXPECTED_ITERATIONS=10000 \
        ORANGEPI_RT_EXPECTED_HOST_NOISE_PCPU="$expected_pcpu" \
        ORANGEPI_RT_SOAK="$soak" \
            bash "$harvest_runner" 2>&1 | tee "$harvest_log"

        finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        receipt_arguments=(
            write-receipt
            --workspace "$workspace"
            --preregistration "$preregistration"
            --result-root "$result_root"
            --phase "$phase"
            --profile "$profile"
            --stage-log "$stage_log"
            --console-log "$console_log"
            --harvest-log "$harvest_log"
            --summary "$summary"
            --raw "$raw"
            --guest-irq "$guest_irq"
            --host-trace "$host_trace"
            --started-at "$started_at"
            --finished-at "$finished_at"
        )
        if [[ "$phase" == pair ]]; then
            receipt_arguments+=(--pair "$pair")
        fi
        python3 "$contract" "${receipt_arguments[@]}"
        completed=1
        rm -f -- "$attempt_dir/attempt-status.log"
        trap - EXIT HUP INT TERM
        echo "AXVISOR_RT_FORMAL_SLOT_COMPLETE phase=$phase pair=${pair:-none} profile=$profile attempt=$attempt_id"
        ;;
    run-all)
        while true; do
            status_json=$(python3 "$contract" status \
                --workspace "$workspace" \
                --preregistration "$preregistration" \
                --result-root "$result_root")
            if [[ $(jq -r '.next == null' <<<"$status_json") == true ]]; then
                break
            fi
            ORANGEPI_SUDO_PASSWORD="${formal_sudo_password:-orangepi}" \
                bash "$0" run-next --result-dir "$result_root"
        done
        bash "$0" aggregate --result-dir "$result_root"
        ;;
    aggregate)
        [[ -r "$preregistration" ]] || fail "preregistration is missing"
        python3 "$contract" verify \
            --workspace "$workspace" \
            --preregistration "$preregistration"
        status_json=$(python3 "$contract" status \
            --workspace "$workspace" \
            --preregistration "$preregistration" \
            --result-root "$result_root")
        [[ $(jq -r '.next == null' <<<"$status_json") == true ]] || \
            fail "all twelve formal slots must complete before aggregation"
        comparisons=()
        for pair in 1 2 3 4 5; do
            pair_root=$result_root/pair-$pair
            shared_receipt=$pair_root/shared/receipt.json
            partitioned_receipt=$pair_root/partitioned/receipt.json
            [[ -r "$shared_receipt" && -r "$partitioned_receipt" ]] || \
                fail "pair $pair receipts are incomplete"
            shared_summary=$(receipt_summary_path "$result_root" "$shared_receipt")
            partitioned_summary=$(receipt_summary_path "$result_root" "$partitioned_receipt")
            temporary=$(mktemp "$pair_root/.comparison.XXXXXX.json")
            python3 "$pair_comparator" \
                "$shared_summary" "$partitioned_summary" --output "$temporary"
            comparison=$pair_root/comparison.json
            write_idempotent_output "$temporary" "$comparison"
            comparisons+=("$comparison")
        done
        shared_soak=$(receipt_summary_path "$result_root" "$result_root/soak/shared/receipt.json")
        partitioned_soak=$(receipt_summary_path "$result_root" "$result_root/soak/partitioned/receipt.json")
        temporary=$(mktemp "$result_root/.campaign-summary.XXXXXX.json")
        python3 "$campaign_aggregator" \
            "${comparisons[@]}" \
            --shared-soak "$shared_soak" \
            --partitioned-soak "$partitioned_soak" \
            --output "$temporary"
        write_idempotent_output "$temporary" "$result_root/campaign-summary.json"
        checksum_temporary=$(mktemp "${TMPDIR:-/tmp}/axvisor-rt-checksums.XXXXXX")
        (
            cd "$result_root"
            find . -type f ! -name checksums.sha256 -print0 | \
                sort -z | xargs -0 sha256sum >"$checksum_temporary"
        )
        write_idempotent_output "$checksum_temporary" "$result_root/checksums.sha256"
        jq -e '.assessment.m2_exit_gate_met == true' \
            "$result_root/campaign-summary.json" >/dev/null || \
            fail "formal campaign completed but the frozen M2 gate failed"
        echo "AXVISOR_RT_FORMAL_CAMPAIGN_COMPLETE result_root=$result_root"
        ;;
    *)
        echo "unsupported formal campaign command: $action" >&2
        usage >&2
        exit 2
        ;;
esac
