#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
    echo "usage: $0 <candidates> <output> <x86_64.log> <aarch64.log> <riscv64.log> <loongarch64.log>" >&2
    exit 2
fi

candidates=$1
output=$2
shift 2

work_dir=$(mktemp -d /tmp/starry-ltp-syscalls-common.XXXXXX)
cleanup()
{
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

awk '/^[A-Za-z0-9_]+$/ { print }' "$candidates" | sort -u > "$work_dir/intersection"

log_index=0
for log_path in "$@"; do
    normalized_log="$work_dir/log-$log_index"
    tr -d '\r' < "$log_path" > "$normalized_log"
    if ! grep -q '^STARRY_SYSTEM_PHASE_BEGIN: ltp-syscalls$' "$normalized_log" ||
        ! grep -q '^STARRY_SYSTEM_PHASE_END: ltp-syscalls$' "$normalized_log"; then
        echo "incomplete LTP phase in $log_path" >&2
        exit 1
    fi
    sed -n 's#^STARRY_SYSTEM_TEST_PASSED: /usr/bin/starry-test-suit/ltp-syscalls-\([^ ]*\) elapsed_s=.*#\1#p' \
        "$normalized_log" | sort -u > "$work_dir/passed-$log_index"
    awk '
        /^STARRY_SYSTEM_TEST_BEGIN: \/usr\/bin\/starry-test-suit\/ltp-syscalls-/ {
            testcase = $0
            sub(/^.*\/ltp-syscalls-/, "", testcase)
            next
        }
        /T(CONF|BROK|FAIL)[[:space:]]*:/ {
            if (testcase != "")
                print testcase
        }
    ' "$normalized_log" | sort -u > "$work_dir/nonpassing-$log_index"
    comm -23 "$work_dir/passed-$log_index" "$work_dir/nonpassing-$log_index" \
        > "$work_dir/eligible-$log_index"
    comm -12 "$work_dir/intersection" "$work_dir/eligible-$log_index" \
        > "$work_dir/next-intersection"
    mv "$work_dir/next-intersection" "$work_dir/intersection"
    log_index=$((log_index + 1))
done

if [[ ! -s "$work_dir/intersection" ]]; then
    echo "the four-architecture LTP intersection is empty" >&2
    exit 1
fi

output_tmp="${output}.tmp"
{
    echo "# LTP 20260529 cases returning zero without TCONF/TBROK/TFAIL on all four architectures."
    cat "$work_dir/intersection"
} > "$output_tmp"
mv "$output_tmp" "$output"
