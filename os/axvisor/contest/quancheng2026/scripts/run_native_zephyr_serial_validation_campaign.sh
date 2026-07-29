#!/usr/bin/env bash

set -euo pipefail

workspace="${1:-/home/kali/qc-zephyrproject}"
rounds="${2:-6}"
duration_seconds="${3:-24}"
stamp_prefix="${4:-$(date +%Y-%m-%d)-native-zsock}"
runner="${5:-/tmp/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh}"
udp_probe="${6:-/tmp/qc-udp-echo-probe.py}"
build_dir="${7:-${workspace}/build/echo_server_virtio_net_bus23_fixed_0x90000000_mgmt_stack_2048_native_zsock}"
summary_log="${8:-/tmp/${stamp_prefix}-campaign-summary.tsv}"
gdb_base_port="${9:-}"

if pgrep -f '[q]emu-system-aarch64' >/dev/null; then
    echo "A QEMU process is already running; refusing concurrent execution." >&2
    exit 10
fi

for path in "${runner}" "${udp_probe}" "${build_dir}/zephyr/zephyr.elf"; do
    if [[ ! -e "${path}" ]]; then
        echo "Required path is missing: ${path}" >&2
        exit 11
    fi
done

printf 'round\tstatus\tresult\tudp_successes\tudp_failures\tuptime_samples\tfatal_marker\tqemu_alive_after_test\trunner_log\tserial_log\tudp_log\tgdb_log\n' \
    >"${summary_log}"

passed=0
for round in $(seq 1 "${rounds}"); do
    stamp="${stamp_prefix}-r${round}"
    host_udp_port="$((21240 + round))"
    serial_port="$((21440 + round))"
    runner_log="/tmp/${stamp}-campaign-runner.log"
    runner_args=(
        "${workspace}"
        "${udp_probe}"
        "${duration_seconds}"
        "${stamp}"
        "${host_udp_port}"
        "${serial_port}"
        "${build_dir}"
    )
    if [[ -n "${gdb_base_port}" ]]; then
        runner_args+=("$((gdb_base_port + round))")
    fi

    echo "=== Native zsock validation round ${round}/${rounds} stamp=${stamp} ==="
    set +e
    "${runner}" "${runner_args[@]}" 2>&1 | tee "${runner_log}"
    status="${PIPESTATUS[0]}"
    set -e

    result="$(sed -n 's/^result=//p' "${runner_log}" | tail -1)"
    serial_log="$(sed -n 's/^serial_log=//p' "${runner_log}" | tail -1)"
    udp_log="$(sed -n 's/^udp_log=//p' "${runner_log}" | tail -1)"
    uptime_samples="$(sed -n 's/^uptime_samples=//p' "${runner_log}" | tail -1)"
    fatal_marker="$(sed -n 's/^fatal_marker=//p' "${runner_log}" | tail -1)"
    qemu_alive="$(sed -n 's/^qemu_alive_after_test=//p' "${runner_log}" | tail -1)"
    gdb_log="$(sed -n 's/^gdb_log=//p' "${runner_log}" | tail -1)"
    udp_successes=""
    udp_failures=""

    if [[ -n "${udp_log}" && -f "${udp_log}" ]]; then
        udp_successes="$(
            sed -n 's/^summary .*successes=\([0-9][0-9]*\).*/\1/p' "${udp_log}" |
                tail -1
        )"
        udp_failures="$(
            sed -n 's/^summary .*failures=\([0-9][0-9]*\).*/\1/p' "${udp_log}" |
                tail -1
        )"
    fi

    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "${round}" \
        "${status}" \
        "${result:-UNKNOWN}" \
        "${udp_successes:-UNKNOWN}" \
        "${udp_failures:-UNKNOWN}" \
        "${uptime_samples:-UNKNOWN}" \
        "${fatal_marker:-UNKNOWN}" \
        "${qemu_alive:-UNKNOWN}" \
        "${runner_log}" \
        "${serial_log:-UNKNOWN}" \
        "${udp_log:-UNKNOWN}" \
        "${gdb_log:-NONE}" \
        >>"${summary_log}"

    echo "round=${round} status=${status} result=${result:-UNKNOWN} udp=${udp_successes:-UNKNOWN}/20 failures=${udp_failures:-UNKNOWN} uptime_samples=${uptime_samples:-UNKNOWN} fatal_marker=${fatal_marker:-UNKNOWN} qemu_alive_after_test=${qemu_alive:-UNKNOWN}"

    if [[ "${status}" -ne 0 || "${result}" != "PASS" ]]; then
        echo "summary_log=${summary_log}"
        echo "passed_rounds=${passed}/${rounds}"
        echo "failed_round=${round}"
        echo "result=FAIL"
        exit 1
    fi

    passed="$((passed + 1))"
    sleep 1
done

echo "summary_log=${summary_log}"
echo "passed_rounds=${passed}/${rounds}"
echo "result=PASS"
