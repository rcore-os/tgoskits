url='${sessionFile:usr/bin/block-rw-bench}'
program=/tmp/block-rw-bench
download=/tmp/block-rw-bench.part
network_ready=0
downloaded=0

export BLOCK_RW_BENCH_ROOT_DEVICE="${BLOCK_RW_BENCH_ROOT_DEVICE:-}"
export BLOCK_RW_BENCH_CONTROLLER="${BLOCK_RW_BENCH_CONTROLLER:-}"
export BLOCK_RW_BENCH_SUCCESS_MARKER="${BLOCK_RW_BENCH_SUCCESS_MARKER:-BLOCK_RW_BENCH_PASSED}"
export BLOCK_RW_BENCH_MAX_TRANSFER_BYTES="${BLOCK_RW_BENCH_MAX_TRANSFER_BYTES:-}"
export BLOCK_RW_BENCH_INLINE_FALLBACK="${BLOCK_RW_BENCH_INLINE_FALLBACK:-0}"
export BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS="${BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS:-30}"
marker="${BLOCK_RW_BENCH_SUCCESS_MARKER%_PASSED}_SESSION"
echo "block-rw-bench: root_device=$BLOCK_RW_BENCH_ROOT_DEVICE controller=$BLOCK_RW_BENCH_CONTROLLER"
rm -f "$program" "$download"

run_inline_case() {
  case_name="$1"
  block_size="$2"
  total_bytes="$3"
  case_path="/root/block-rw-bench/inline-${case_name}.bin"
  block_count=$(( (total_bytes + block_size - 1) / block_size ))
  actual_bytes=$(( block_count * block_size ))
  start_seconds="$(date +%s)"
  if ! dd if=/dev/zero of="$case_path" bs="$block_size" count="$block_count" conv=fsync; then
    echo "block-rw-bench:" "error: inline write failed for $case_name"
    return 1
  fi
  elapsed_seconds=$(( $(date +%s) - start_seconds ))
  expected="$(dd if=/dev/zero bs="$block_size" count="$block_count" 2>/dev/null | cksum)"
  actual="$(cksum "$case_path")"
  set -- $expected
  expected_crc="$1"
  expected_bytes="$2"
  set -- $actual
  actual_crc="$1"
  actual_size="$2"
  if [ "$expected_crc" != "$actual_crc" ] ||
     [ "$expected_bytes" != "$actual_size" ] ||
     [ "$actual_bytes" != "$actual_size" ]; then
    echo "block-rw-bench:" "error: inline verify mismatch for $case_name"
    return 1
  fi
  echo "block-rw-bench: case=$case_name io_size=$block_size bytes=$actual_bytes elapsed_s=$elapsed_seconds fsync=ok verify=ok"
  rm -f "$case_path"
}

run_inline_multitask() {
  worker_pids=""
  worker=0
  while [ "$worker" -lt 8 ]; do
    dd if=/dev/zero of="/root/block-rw-bench/inline-worker-${worker}.bin" \
      bs=4096 count=128 conv=fsync >/dev/null 2>&1 &
    worker_pids="$worker_pids $!"
    worker=$(( worker + 1 ))
  done
  for worker_pid in $worker_pids; do
    if ! wait "$worker_pid"; then
      echo "block-rw-bench:" "error: inline multitask write failed"
      return 1
    fi
  done
  expected="$(dd if=/dev/zero bs=4096 count=128 2>/dev/null | cksum)"
  set -- $expected
  expected_crc="$1"
  expected_bytes="$2"
  worker=0
  while [ "$worker" -lt 8 ]; do
    worker_path="/root/block-rw-bench/inline-worker-${worker}.bin"
    actual="$(cksum "$worker_path")"
    set -- $actual
    if [ "$1" != "$expected_crc" ] || [ "$2" != "$expected_bytes" ]; then
      echo "block-rw-bench:" "error: inline multitask verify mismatch for worker $worker"
      return 1
    fi
    rm -f "$worker_path"
    worker=$(( worker + 1 ))
  done
  echo "block-rw-bench: case=multitask tasks=8 io_size=4096 bytes_per_task=524288 fsync=ok verify=ok"
}

run_inline_fallback() {
  root_source="$(awk '$2 == "/" { print $1; exit }' /proc/mounts)"
  case "$root_source" in
    "$BLOCK_RW_BENCH_ROOT_DEVICE"|"$BLOCK_RW_BENCH_ROOT_DEVICE"p[0-9]*)
      ;;
    *)
      echo "block-rw-bench:" "error: root-device mismatch: expected $BLOCK_RW_BENCH_ROOT_DEVICE, found $root_source"
      return 1
      ;;
  esac
  if ! command -v dd >/dev/null 2>&1 ||
     ! command -v cksum >/dev/null 2>&1 ||
     ! command -v awk >/dev/null 2>&1; then
    echo "block-rw-bench:" "error: inline fallback requires dd, cksum, and awk"
    return 1
  fi
  rm -rf /root/block-rw-bench
  mkdir -p /root/block-rw-bench || return 1
  max_transfer="$BLOCK_RW_BENCH_MAX_TRANSFER_BYTES"
  planner_split=$(( max_transfer + 512 ))
  echo "block-rw-bench: mode=serial-inline root_device=$root_source controller=$BLOCK_RW_BENCH_CONTROLLER status=ok"
  run_inline_case sector 512 2097152 &&
    run_inline_case page 4096 2097152 &&
    run_inline_case hardware-max "$max_transfer" 4194304 &&
    run_inline_case planner-split "$planner_split" 4194304 &&
    run_inline_multitask &&
    sync &&
    echo "$BLOCK_RW_BENCH_SUCCESS_MARKER"
}

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
  if [ "$BLOCK_RW_BENCH_INLINE_FALLBACK" = "1" ]; then
    run_inline_fallback || echo "${marker}_FAILED"
  else
    echo "${marker}_FAILED"
  fi
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
