#!/usr/bin/env bash
# Exercise the real board init script with a transfer that leaves partial data.
set -euo pipefail
app_dir=$(cd -- "$(dirname -- "$0")" && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf -- "$test_dir"' EXIT
mkdir -p "$test_dir/bin" "$test_dir/tmp"

# Generate only the external transfer primitive, not the download policy.
printf '%s\n' '#!/bin/sh' \
    'while [ "$#" -gt 0 ]; do' \
    '  if [ "$1" = "-o" ]; then shift; output=$1; fi' \
    '  shift' \
    'done' \
    'printf "%s\n" "#!/bin/sh" "echo STARRY_LINUX_PERF_PASSED" > "$output"' \
    'if [ "${TRANSFER_RETRY_ONCE:-0}" = 1 ]; then' \
    '  if [ -e "$output.attempted" ]; then exit 0; fi' \
    '  touch "$output.attempted"' \
    'fi' \
    'exit 18' > "$test_dir/bin/curl"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$test_dir/bin/sleep"
chmod +x "$test_dir/bin/curl" "$test_dir/bin/sleep"

PATH="$test_dir/bin:$PATH" TMPDIR="$test_dir/tmp" \
    STARRY_LINUX_PERF_ARCHIVE_0_URL=fixture://archive0 \
    STARRY_LINUX_PERF_ARCHIVE_1_URL=fixture://archive1 \
    STARRY_LINUX_PERF_RUNNER_URL=fixture://runner \
    STARRY_LINUX_PERF_WORKLOAD_URL=fixture://workload \
    sh "$app_dir/init.sh" > "$test_dir/output" 2>&1 || true

if grep -q '^STARRY_LINUX_PERF_PASSED$' "$test_dir/output"; then
    echo 'FAIL: board init executed an incomplete download'
    exit 1
fi
grep -q '^STARRY_LINUX_PERF_FAILED: session assets$' "$test_dir/output"
for file in runtime.tar.gz.part-0 runtime.tar.gz.part-1 linux-perf-run linux-perf-workload; do
    if [ -e "$test_dir/tmp/starry-linux-perf-session/$file" ]; then
        echo "FAIL: incomplete transfer published $file"
        exit 1
    fi
done

# A partial first attempt must also recover when the next transfer succeeds.
PATH="$test_dir/bin:$PATH" TMPDIR="$test_dir/tmp" TRANSFER_RETRY_ONCE=1 \
    STARRY_LINUX_PERF_ARCHIVE_0_URL=fixture://archive0 \
    STARRY_LINUX_PERF_ARCHIVE_1_URL=fixture://archive1 \
    STARRY_LINUX_PERF_RUNNER_URL=fixture://runner \
    STARRY_LINUX_PERF_WORKLOAD_URL=fixture://workload \
    sh "$app_dir/init.sh" > "$test_dir/recovered" 2>&1
grep -q '^STARRY_LINUX_PERF_PASSED$' "$test_dir/recovered"
for file in runtime.tar.gz.part-0 runtime.tar.gz.part-1 linux-perf-run linux-perf-workload; do
    [ -s "$test_dir/tmp/starry-linux-perf-session/$file" ]
    [ -e "$test_dir/tmp/starry-linux-perf-session/$file.part.attempted" ]
done
echo 'LINUX_PERF_DOWNLOAD_TEST_OK'
