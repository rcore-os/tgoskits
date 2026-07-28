marker=ORANGEPI_BLOCK_RW_BENCH_SESSION
url='${sessionFile:usr/bin/block-rw-bench}'
program=/tmp/block-rw-bench
download=/tmp/block-rw-bench.part
network_ready=0
downloaded=0
rm -f "$program" "$download"
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do
  ip_addr_out="$(ip addr show dev eth0 2>&1 || true)"
  echo "$ip_addr_out"
  if echo "$ip_addr_out" | grep -q 'inet 192\.168\.1\.'; then
    network_ready=1
    break
  fi
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
