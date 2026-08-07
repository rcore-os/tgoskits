url='${sessionFile:usr/bin/block-rw-bench}'
program="${BLOCK_RW_BENCH_PROGRAM:-/tmp/block-rw-bench}"
download="${program}.part"
helper_log="${program}.log"
staged_program="${BLOCK_RW_BENCH_STAGED_PROGRAM:-/usr/local/libexec/block-rw-bench}"
workdir="${BLOCK_RW_BENCH_WORKDIR:-/root/block-rw-bench}"
download_attempts="${BLOCK_RW_BENCH_DOWNLOAD_ATTEMPTS:-6}"
download_retry_seconds="${BLOCK_RW_BENCH_DOWNLOAD_RETRY_SECONDS:-5}"
helper_program=
helper_ready=0
helper_passed=0

export BLOCK_RW_BENCH_ROOT_DEVICE="${BLOCK_RW_BENCH_ROOT_DEVICE:-}"
export BLOCK_RW_BENCH_CONTROLLER="${BLOCK_RW_BENCH_CONTROLLER:-}"
export BLOCK_RW_BENCH_SUCCESS_MARKER="${BLOCK_RW_BENCH_SUCCESS_MARKER:-BLOCK_RW_BENCH_PASSED}"
export BLOCK_RW_BENCH_MAX_TRANSFER_BYTES="${BLOCK_RW_BENCH_MAX_TRANSFER_BYTES:-}"
marker="${BLOCK_RW_BENCH_SUCCESS_MARKER%_PASSED}_SESSION"
echo "block-rw-bench: root_device=$BLOCK_RW_BENCH_ROOT_DEVICE controller=$BLOCK_RW_BENCH_CONTROLLER"
rm -f "$download" "$helper_log"
if [ "$program" != "$staged_program" ]; then
  rm -f "$program"
fi

case "$download_attempts" in
  ''|*[!0-9]*|0) download_attempts=0 ;;
esac
case "$download_retry_seconds" in
  ''|*[!0-9]*) download_retry_seconds=0 ;;
esac

if [ -s "$staged_program" ]; then
  helper_program="$staged_program"
  helper_ready=1
  echo "block-rw-bench: helper=linux-staged"
else
  attempt=1
  while [ "$attempt" -le "$download_attempts" ]; do
    rm -f "$download"
    if command -v curl >/dev/null 2>&1; then
      curl --connect-timeout 10 --max-time 30 -fsSL "$url" -o "$download" || true
    elif command -v wget >/dev/null 2>&1; then
      wget -T 30 -O "$download" "$url" || true
    else
      break
    fi
    if [ -s "$download" ]; then
      helper_ready=1
      echo "block-rw-bench: helper=session-http attempt=$attempt"
      break
    fi
    attempt=$(( attempt + 1 ))
    if [ "$attempt" -le "$download_attempts" ] && [ "$download_retry_seconds" -gt 0 ]; then
      sleep "$download_retry_seconds"
    fi
  done
  if [ "$helper_ready" = "1" ] &&
    mv "$download" "$program" &&
    chmod +x "$program"; then
    helper_program="$program"
  else
    helper_ready=0
  fi
fi

if [ "$helper_ready" = "1" ] &&
  mkdir -p "$workdir"; then
  export BLOCK_RW_BENCH_WORKDIR="$workdir"
  if "$helper_program" >"$helper_log" 2>&1; then
    cat "$helper_log"
    if grep -Fqx "$BLOCK_RW_BENCH_SUCCESS_MARKER" "$helper_log" && sync; then
      helper_passed=1
    fi
  else
    cat "$helper_log"
  fi
fi

if [ "$helper_passed" != "1" ]; then
  printf '\n%s\n' "${marker}_FAILED"
fi
