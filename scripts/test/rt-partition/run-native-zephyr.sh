#!/usr/bin/env bash
set -euo pipefail

# Run the periodic sampler directly under QEMU, without Axvisor, and archive
# the same CSV/statistics evidence used by the RT-partition experiment.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
input_dir="${NATIVE_ZEPHYR_INPUT_DIR:-${repo_root}/tmp/rt-partition/native-zephyr}"
out_dir="${NATIVE_ZEPHYR_OUTPUT_DIR:-${repo_root}/results/task1/native-zephyr}"
qemu_bin="${QEMU_BIN:-qemu-system-aarch64}"
timeout_sec="${NATIVE_ZEPHYR_TIMEOUT_SEC:-30}"
input_dir="$(realpath -m "$input_dir")"
out_dir="$(realpath -m "$out_dir")"

[[ "$timeout_sec" =~ ^[0-9]+$ ]] && (( timeout_sec > 0 )) || {
    printf 'error: NATIVE_ZEPHYR_TIMEOUT_SEC must be a positive integer\n' >&2
    exit 2
}
command -v "$qemu_bin" >/dev/null 2>&1 || {
    printf 'error: QEMU executable not found: %s\n' "$qemu_bin" >&2
    exit 1
}

input_elf="${input_dir}/zephyr-periodic.elf"
input_bin="${input_dir}/zephyr-periodic.bin"
input_manifest="${input_dir}/zephyr-periodic.manifest"
for path in "$input_elf" "$input_bin" "$input_manifest"; do
    [[ -f "$path" ]] || {
        printf 'error: missing native Zephyr input: %s\n' "$path" >&2
        printf 'build it with ZEPHYR_MEMORY_BASE=0x40000000 build-zephyr-periodic.sh\n' >&2
        exit 1
    }
done

linked_base="$(sed -n 's/^linked_base=//p' "$input_manifest")"
sample_count="$(sed -n 's/^sample_count=//p' "$input_manifest")"
start_gated="$(sed -n 's/^start_gated=//p' "$input_manifest")"
expected_sha="$(sed -n 's/^sha256=//p' "$input_manifest")"
actual_sha="$(sha256sum "$input_bin" | awk '{print $1}')"
[[ "$linked_base" =~ ^0x[0-9a-fA-F]+$ ]] || {
    printf 'error: invalid linked_base in native Zephyr manifest: %s\n' "$linked_base" >&2
    exit 1
}
(( linked_base == 0x40000000 )) || {
    printf 'error: native Zephyr image is linked at %s, expected 0x40000000\n' "$linked_base" >&2
    exit 1
}
[[ "$sample_count" == "300" ]] || {
    printf 'error: native Zephyr manifest sample count is %s, expected 300\n' "$sample_count" >&2
    exit 1
}
[[ "$start_gated" == "0" ]] || {
    printf 'error: native Zephyr image must be built with ZEPHYR_START_GATED=0\n' >&2
    exit 1
}
[[ "$actual_sha" == "$expected_sha" ]] || {
    printf 'error: native Zephyr raw image does not match its manifest\n' >&2
    exit 1
}

mkdir -p "$out_dir"
rm -f "$out_dir/raw.log" "$out_dir/zephyr.csv" "$out_dir/stats.txt" \
    "$out_dir/meta.txt" "$out_dir/qemu-command.txt" "$out_dir/sha256sums" \
    "$out_dir/zephyr-periodic.elf" "$out_dir/zephyr-periodic.bin" \
    "$out_dir/zephyr-periodic.manifest"
cp "$input_elf" "$out_dir/zephyr-periodic.elf"
cp "$input_bin" "$out_dir/zephyr-periodic.bin"
cp "$input_manifest" "$out_dir/zephyr-periodic.manifest"

qemu_args=(
    -cpu cortex-a72
    -nographic
    -machine virt,gic-version=3
    -net none
    -kernel "$out_dir/zephyr-periodic.elf"
)
printf '%q ' "$qemu_bin" "${qemu_args[@]}" > "$out_dir/qemu-command.txt"
printf '\n' >> "$out_dir/qemu-command.txt"

start_ns="$(date +%s%N)"
set +e
timeout --signal=INT --kill-after=5 "$timeout_sec" \
    "$qemu_bin" "${qemu_args[@]}" > "$out_dir/raw.log" 2>&1
qemu_status=$?
set -e
end_ns="$(date +%s%N)"
elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))

case "$qemu_status" in
    0|124|130) ;;
    *)
        printf 'error: native QEMU exited with status %s\n' "$qemu_status" >&2
        tail -80 "$out_dir/raw.log" >&2
        exit 1
        ;;
esac
rg -F "PERIODIC LATENCY COMPLETE samples=300" "$out_dir/raw.log" >/dev/null || {
    printf 'error: native Zephyr did not reach PERIODIC LATENCY COMPLETE samples=300\n' >&2
    tail -80 "$out_dir/raw.log" >&2
    exit 1
}

python3 - "$out_dir/raw.log" "$out_dir/zephyr.csv" <<'PY'
import csv
import re
import sys
from pathlib import Path

log_path = Path(sys.argv[1])
csv_path = Path(sys.argv[2])
log = log_path.read_text(errors="replace")
header = "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns"
header_index = log.find(header)
complete_index = log.find("PERIODIC LATENCY COMPLETE samples=300", header_index)
if header_index < 0 or complete_index < header_index:
    raise SystemExit("native Zephyr CSV markers are missing")

rows = []
ansi_escape = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
for line in log[header_index + len(header):complete_index].splitlines():
    candidate = ansi_escape.sub("", line.strip())
    match = re.search(r"(\d+,-?\d+,-?\d+,-?\d+,-?\d+)$", candidate)
    if match:
        rows.append(match.group(1).split(","))
if len(rows) != 300:
    raise SystemExit(f"expected 300 native Zephyr samples, found {len(rows)}")
if [int(row[0]) for row in rows] != list(range(300)):
    raise SystemExit("native Zephyr sample sequence is incomplete or out of order")

with csv_path.open("w", newline="") as stream:
    writer = csv.writer(stream)
    writer.writerow(header.split(","))
    writer.writerows(rows)
PY

python3 "$repo_root/scripts/test/rt_latency_stats.py" \
    "$out_dir/zephyr.csv" > "$out_dir/stats.txt"

{
    printf 'environment=native-qemu-without-axvisor\n'
    printf 'git_commit=%s\n' "$(git -C "$repo_root" rev-parse HEAD)"
    printf 'qemu_version=%s\n' "$("$qemu_bin" --version | head -n1)"
    printf 'start_ns=%s\n' "$start_ns"
    printf 'end_ns=%s\n' "$end_ns"
    printf 'elapsed_ms=%s\n' "$elapsed_ms"
    printf 'timeout_sec=%s\n' "$timeout_sec"
    printf 'qemu_exit_status=%s\n' "$qemu_status"
    printf 'timing_model=wall-clock TCG\n'
    printf 'claim_scope=QEMU TCG trend only; sub-50us effects are not hardware-real-time evidence\n'
} > "$out_dir/meta.txt"

(
    cd "$out_dir"
    sha256sum raw.log zephyr.csv stats.txt meta.txt qemu-command.txt \
        zephyr-periodic.elf zephyr-periodic.bin zephyr-periodic.manifest > sha256sums
)

printf 'accepted native Zephyr evidence: %s\n' "$out_dir"
