#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="/home/kali/qc-zephyrproject"
board="qemu_cortex_a53"
evidence_dir="/tmp/$(date +%Y-%m-%d_%H-%M-%S)-native-zephyr-latency"
build_dir=""
run_timeout_seconds=35
skip_build=0

usage() {
    cat <<'EOF'
Usage: run_native_zephyr_latency_baseline.sh [options]

Options:
  --workspace PATH        Zephyr workspace. Default: /home/kali/qc-zephyrproject.
  --board NAME            Zephyr board. Default: qemu_cortex_a53.
  --build-dir PATH        Build directory. Default: <workspace>/build/qc_latency_measure_<board>.
  --evidence-dir PATH     Output evidence directory.
  --run-timeout SECONDS   Timeout for west run. Default: 35.
  --skip-build            Reuse an existing build directory.
  -h, --help              Show this help.

The script builds and runs Zephyr's official tests/benchmarks/latency_measure
as a native RTOS baseline for the contest realtime comparison.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --workspace)
            workspace="$2"
            shift 2
            ;;
        --board)
            board="$2"
            shift 2
            ;;
        --build-dir)
            build_dir="$2"
            shift 2
            ;;
        --evidence-dir)
            evidence_dir="$2"
            shift 2
            ;;
        --run-timeout)
            run_timeout_seconds="$2"
            shift 2
            ;;
        --skip-build)
            skip_build=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

workspace="$(cd "${workspace}" && pwd)"
zephyr_base="${workspace}/zephyr"
app="${zephyr_base}/tests/benchmarks/latency_measure"
build_dir="${build_dir:-${workspace}/build/qc_latency_measure_${board//\//_}}"
env_file="${workspace}/env.sh"
analyzer="${script_dir}/analyze_zephyr_latency_measure.py"

mkdir -p "${evidence_dir}"
exec > >(tee "${evidence_dir}/runner.log") 2>&1
: > "${evidence_dir}/build.log"

echo "experiment=Zephyr native latency baseline"
echo "workspace=${workspace}"
echo "zephyr_base=${zephyr_base}"
echo "app=${app}"
echo "board=${board}"
echo "build_dir=${build_dir}"
echo "evidence_dir=${evidence_dir}"

for required in timeout python3 grep pgrep; do
    if ! command -v "${required}" >/dev/null 2>&1; then
        echo "missing_required_tool=${required}" >&2
        exit 10
    fi
done

if [[ ! -f "${env_file}" ]]; then
    echo "missing_env_file=${env_file}" >&2
    exit 11
fi
if [[ ! -d "${app}" ]]; then
    echo "missing_latency_measure_app=${app}" >&2
    exit 12
fi
if [[ ! -x "${analyzer}" ]]; then
    echo "missing_analyzer=${analyzer}" >&2
    exit 13
fi
if pgrep -f '[q]emu-system-aarch64' >/dev/null; then
    echo "A QEMU process is already running; refusing concurrent execution." >&2
    pgrep -af '[q]emu-system-aarch64' >&2 || true
    exit 14
fi

# shellcheck disable=SC1090
source "${env_file}"
if ! command -v west >/dev/null 2>&1; then
    echo "missing_required_tool=west" >&2
    exit 15
fi

cd "${zephyr_base}"
zephyr_version="$(git describe --tags --always --dirty 2>/dev/null || git rev-parse --short HEAD)"
west_version="$(west --version)"
echo "zephyr_version=${zephyr_version}"
echo "west_version=${west_version}"

if [[ "${skip_build}" -eq 0 ]]; then
    echo "--- build latency_measure ---"
    set +e
    west build -p always -b "${board}" -d "${build_dir}" "${app}" \
        >"${evidence_dir}/build.log" 2>&1
    build_status=$?
    set -e
    echo "build_status=${build_status}"
    tail -30 "${evidence_dir}/build.log" || true
    if [[ "${build_status}" -ne 0 ]]; then
        echo "result=FAIL"
        exit "${build_status}"
    fi
else
    echo "build_status=SKIPPED"
fi

if [[ ! -f "${build_dir}/zephyr/zephyr.elf" ]]; then
    echo "missing_build_output=${build_dir}/zephyr/zephyr.elf" >&2
    echo "result=FAIL"
    exit 16
fi

echo "--- run latency_measure ---"
set +e
timeout --signal=TERM --kill-after=5s "${run_timeout_seconds}" \
    west build -d "${build_dir}" -t run \
    >"${evidence_dir}/run.log" 2>&1
run_status=$?
set -e
echo "run_status=${run_status}"

success_marker=0
if grep -aq 'PROJECT EXECUTION SUCCESSFUL' "${evidence_dir}/run.log"; then
    success_marker=1
fi
metric_count="$(grep -aEc '^[A-Za-z0-9_.+]+[[:space:]]+- .*:[[:space:]]+[0-9]+ cycles ,[[:space:]]+[0-9]+ ns' "${evidence_dir}/run.log" || true)"
qemu_alive_after_run=0
if pgrep -f '[q]emu-system-aarch64' >/dev/null; then
    qemu_alive_after_run=1
fi

echo "success_marker=${success_marker}"
echo "metric_count=${metric_count}"
echo "qemu_alive_after_run=${qemu_alive_after_run}"

python3 "${analyzer}" "${evidence_dir}/run.log" --fail-on-missing

{
    echo "experiment=Zephyr native latency baseline"
    echo "workspace=${workspace}"
    echo "zephyr_base=${zephyr_base}"
    echo "zephyr_version=${zephyr_version}"
    echo "west_version=${west_version}"
    echo "board=${board}"
    echo "build_dir=${build_dir}"
    echo "evidence_dir=${evidence_dir}"
    echo "run_timeout_seconds=${run_timeout_seconds}"
    echo "run_status=${run_status}"
    echo "success_marker=${success_marker}"
    echo "metric_count=${metric_count}"
    echo "qemu_alive_after_run=${qemu_alive_after_run}"
    grep -aE 'thread.yield.preemptive|isr.resume.interrupted|isr.resume.different|semaphore.give.wake|semaphore.take.blocking|mutex.lock.immediate|heap.malloc.immediate' \
        "${evidence_dir}/run.log" || true
} >"${evidence_dir}/summary.txt"

sha256sum \
    "${evidence_dir}/build.log" \
    "${evidence_dir}/run.log" \
    "${evidence_dir}/latency-summary.json" \
    "${evidence_dir}/latency-report.md" \
    "${evidence_dir}/summary.txt" \
    >"${evidence_dir}/sha256.txt"

if [[ "${success_marker}" -eq 1 &&
      "${metric_count}" -gt 0 &&
      "${qemu_alive_after_run}" -eq 0 ]]; then
    echo "summary_txt=${evidence_dir}/summary.txt"
    echo "latency_json=${evidence_dir}/latency-summary.json"
    echo "latency_report=${evidence_dir}/latency-report.md"
    echo "result=PASS"
    exit 0
fi

echo "result=FAIL"
exit 1
