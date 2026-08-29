server_ip="${STARRY_IPERF3_SERVER:-}"
script_path="${STARRY_IPERF3_SCRIPT_PATH:-}"
script_url="${STARRY_IPERF3_SCRIPT_URL:-}"
script=/tmp/iperf-bench.sh
download=/tmp/iperf-bench.sh.part
ready=0

if [ -n "$script_path" ] && [ -s "$script_path" ]; then
    script=$script_path
    chmod +x "$script" && ready=1
fi

rm -f "$download"
attempt=1
while [ "$ready" != "1" ] && [ "$attempt" -le 30 ]; do
    if command -v curl >/dev/null 2>&1; then
        curl --connect-timeout 2 --max-time 5 -fsSL "$script_url" -o "$download" || true
    elif command -v wget >/dev/null 2>&1; then
        wget -T 5 -O "$download" "$script_url" || true
    fi

    if [ -s "$download" ] && mv "$download" "$script" && chmod +x "$script"; then
        ready=1
        break
    fi

    sleep 1
    attempt=$((attempt + 1))
done

if [ -z "$server_ip" ] || [ "$ready" != "1" ]; then
    echo STARRY_IPERF3_BENCH_FAILED
elif "$script" "$server_ip"; then
    sync
else
    echo STARRY_IPERF3_BENCH_FAILED
fi
