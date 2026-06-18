#!/bin/sh
set -u

rounds="${BLOCK_BENCH_ROUNDS:-5}"
bytes="${BLOCK_BENCH_BYTES:-4194304}"
block_bytes="${BLOCK_BENCH_BLOCK_BYTES:-4096}"
path="${BLOCK_BENCH_PATH:-/root/block-io-bench-app}"

echo "BLOCK_BENCH_APP_START rounds=$rounds bytes=$bytes block_bytes=$block_bytes path=$path"

/usr/bin/block-io-bench \
    --rounds "$rounds" \
    --bytes "$bytes" \
    --block-bytes "$block_bytes" \
    --path "$path"
status=$?

echo "BLOCK_BENCH_APP_DONE rc=$status"
if [ "$status" -eq 0 ]; then
    echo "BLOCK_BENCH_APP_PASSED"
else
    echo "BLOCK_BENCH_APP_FAILED"
fi
exit "$status"
