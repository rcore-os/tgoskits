url='${sessionFile:usr/bin/block-rw-bench}'
program=/tmp/block-rw-bench
download=/tmp/block-rw-bench.part
network_ready=0
downloaded=0

export BLOCK_RW_BENCH_ROOT_DEVICE="${BLOCK_RW_BENCH_ROOT_DEVICE:-}"
export BLOCK_RW_BENCH_CONTROLLER="${BLOCK_RW_BENCH_CONTROLLER:-}"
export BLOCK_RW_BENCH_SUCCESS_MARKER="${BLOCK_RW_BENCH_SUCCESS_MARKER:-BLOCK_RW_BENCH_PASSED}"
export BLOCK_RW_BENCH_MAX_TRANSFER_BYTES="${BLOCK_RW_BENCH_MAX_TRANSFER_BYTES:-}"
export BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS="${BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS:-30}"
marker="${BLOCK_RW_BENCH_SUCCESS_MARKER%_PASSED}_SESSION"
echo "block-rw-bench: root_device=$BLOCK_RW_BENCH_ROOT_DEVICE controller=$BLOCK_RW_BENCH_CONTROLLER"
rm -f "$program" "$download"

attempt=0
while [ "$attempt" -lt "$BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS" ]; do
  ip_addr_out="$(ip -4 addr show scope global 2>&1 || true)"
  echo "$ip_addr_out"
  if echo "$ip_addr_out" | grep -q 'inet '; then
    network_ready=1
    break
  fi
  attempt=$(( attempt + 1 ))
  sleep 1
done
if [ "$network_ready" != "1" ]; then
  echo "${marker}_FAILED"
elif command -v curl >/dev/null 2>&1; then
  if curl --connect-timeout 10 --max-time 60 -fsSL "$url" -o "$download"; then
    downloaded=1
  else
    echo "${marker}_FAILED"
  fi
elif command -v wget >/dev/null 2>&1; then
  if wget -T 60 -O "$download" "$url"; then
    downloaded=1
  else
    echo "${marker}_FAILED"
  fi
else
  echo "${marker}_FAILED"
fi
if [ "$downloaded" = "1" ] && [ -s "$download" ]; then
  mv "$download" "$program" &&
    chmod +x "$program" &&
    rm -rf /root/block-rw-bench &&
    mkdir -p /root/block-rw-bench &&
    "$program" &&
    sync ||
    echo "${marker}_FAILED"
elif [ "$downloaded" = "1" ]; then
  echo "${marker}_FAILED"
fi
