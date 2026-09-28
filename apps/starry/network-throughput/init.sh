bootstrap_url=${STARRY_NETWORK_BENCH_BOOTSTRAP_URL:-}
bootstrap=/tmp/network-bench-bootstrap.sh
ready=0
attempt=1
while [ "$attempt" -le 30 ]; do
    if [ -n "$bootstrap_url" ] &&
        { { command -v wget >/dev/null 2>&1 &&
            wget -q -T 5 -O "$bootstrap" "$bootstrap_url"; } ||
          { command -v curl >/dev/null 2>&1 &&
            curl --connect-timeout 2 --max-time 5 -fsS "$bootstrap_url" -o "$bootstrap"; }; } &&
        [ -s "$bootstrap" ]; then ready=1; break; fi
    sleep 1
    attempt=$((attempt + 1))
done
if [ "$ready" = 1 ]; then
    sh "$bootstrap"
else
    echo STARRY_NETWORK_BENCH_FAILED
fi
