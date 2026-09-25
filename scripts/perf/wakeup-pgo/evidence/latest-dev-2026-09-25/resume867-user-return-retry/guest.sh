#!/bin/sh
set -eu

sid="$1"
base=http://192.168.1.2:2999
out=/tmp/resume867
mkdir -p "$out"

put() {
  curl -fsS -X PUT -H "X-File-Path: resume866-$1" \
    --data-binary "@$out/$1" "$base/api/v1/sessions/$sid/files" >/dev/null
}

download=1
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if curl --connect-timeout 10 --max-time 20 -fsS \
    "$base/share/sessions/$sid/bench" -o "$out/bench"; then
    download=0
    break
  fi
  sleep 2
done
if [ "$download" -ne 0 ]; then
  echo RESUME866_DONE 7
  exit 7
fi

chmod +x "$out/bench"
sha256sum "$out/bench" > "$out/bench.sha256"
put bench.sha256
if [ ! -f /sys/kernel/debug/user_return_metrics ]; then
  mkdir -p /sys/kernel/debug
  mount -t debugfs debugfs /sys/kernel/debug
fi
test -f /sys/kernel/debug/user_return_metrics

overall=0
for sequence in other-1 fifo-1 other-2 fifo-2 other-3; do
  policy="${sequence%-*}"
  name="$sequence-thread_futex_same_cpu"
  cat /sys/kernel/debug/user_return_metrics > "$out/$name-before"
  status=0
  "$out/bench" --policy "$policy" --case thread_futex_same_cpu \
    > "$out/$name.log" 2>&1 || status=$?
  cat /sys/kernel/debug/user_return_metrics > "$out/$name-after"
  echo "DIAGNOSTIC_EXIT $status" >> "$out/$name.log"
  put "$name.log"
  put "$name-before"
  put "$name-after"
  if [ "$status" -ne 0 ]; then overall=1; fi
done

echo "RESUME866_DONE $overall"
[ "$overall" -eq 0 ]
