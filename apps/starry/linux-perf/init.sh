session_dir=/tmp/starry-linux-perf-session
archive_0="$session_dir/runtime.tar.gz.part-0"
archive_1="$session_dir/runtime.tar.gz.part-1"
runner="$session_dir/linux-perf-run"
workload="$session_dir/linux-perf-workload"
ready=1

rm -rf "$session_dir"
mkdir -p "$session_dir"

download_file() {
    url="$1"
    output="$2"
    partial="$output.part"
    attempt=1
    while [ "$attempt" -le 30 ]; do
        if command -v curl >/dev/null 2>&1; then
            curl --connect-timeout 2 --max-time 20 -fsSL "$url" -o "$partial" || true
        elif command -v wget >/dev/null 2>&1; then
            wget -T 20 -O "$partial" "$url" || true
        fi
        if [ -s "$partial" ] && mv "$partial" "$output"; then
            return 0
        fi
        sleep 1
        attempt=$((attempt + 1))
    done
    return 1
}

download_file "$STARRY_LINUX_PERF_ARCHIVE_0_URL" "$archive_0" || ready=0
download_file "$STARRY_LINUX_PERF_ARCHIVE_1_URL" "$archive_1" || ready=0
download_file "$STARRY_LINUX_PERF_RUNNER_URL" "$runner" || ready=0
download_file "$STARRY_LINUX_PERF_WORKLOAD_URL" "$workload" || ready=0

if [ "$ready" != "1" ]; then
    # Keep the literal failure sentinel out of the injected command text: the
    # board console echoes multiline input, and the monitor must only match an
    # actually executed failure report.
    printf 'STARRY_LINUX_PERF_%s: session assets\n' FAILED
else
    chmod +x "$runner" "$workload"
    LINUX_PERF_BOARD=1 \
        LINUX_PERF_RUNTIME_ARCHIVE_PARTS="$archive_0 $archive_1" \
        LINUX_PERF_WORKLOAD="$workload" \
        "$runner"
fi
