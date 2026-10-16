#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
host=""

usage() {
    echo "Usage: $0 --host BOARD_IP"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) host="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done
[[ "$host" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid --host" >&2; exit 2; }
"$app_dir/provision_rootfs.sh" --host "$host"
batch="baseline-$(date -u +%Y%m%dT%H%M%SZ)"
result_dir="$repo_root/target/starry-orangepi5plus-selfbuild/benchmarks/$batch"
mkdir -p "$result_dir"
samples="$result_dir/samples.tsv"
printf 'system\tround\trun_id\telapsed_seconds\n' > "$samples"

for round in 1 2 3; do
    linux_run="linux-${batch}-${round}"
    "$app_dir/run_linux_baseline.sh" --host "$host" --skip-provision --run-id "$linux_run"
    linux_elapsed="$(cat "$repo_root/target/starry-orangepi5plus-selfbuild/artifacts/$linux_run/elapsed-seconds")"
    printf 'linux\t%s\t%s\t%s\n' "$round" "$linux_run" "$linux_elapsed" >> "$samples"

    starry_run="starry-${batch}-${round}"
    "$app_dir/run_selfbuild.sh" --host "$host" --skip-provision \
        --skip-boot-build --run-id "$starry_run"
    starry_elapsed="$(cat "$repo_root/target/starry-orangepi5plus-selfbuild/artifacts/$starry_run/elapsed-seconds")"
    printf 'starry\t%s\t%s\t%s\n' "$round" "$starry_run" "$starry_elapsed" >> "$samples"
done

linux_median="$(awk -F '\t' '$1 == "linux" { print $4 }' "$samples" | sort -n | sed -n '2p')"
starry_median="$(awk -F '\t' '$1 == "starry" { print $4 }' "$samples" | sort -n | sed -n '2p')"
{
    printf 'linux_median_seconds=%s\n' "$linux_median"
    printf 'starry_median_seconds=%s\n' "$starry_median"
} | tee "$result_dir/summary"
echo "benchmark_result_dir=$result_dir"
