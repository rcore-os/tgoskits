#!/bin/sh
set -eu
sid="$1"
base=http://192.168.1.2:2999
out=/tmp/resume854
mkdir -p "$out"
put() {
  curl -fsS -X PUT -H "X-File-Path: resume854-$1" --data-binary "@$out/$1" "$base/api/v1/sessions/$sid/files" >/dev/null
}
download=1
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if curl --connect-timeout 10 --max-time 20 -fsS "$base/share/sessions/$sid/bench" -o "$out/bench"; then
    download=0
    break
  fi
  sleep 2
done
if [ "$download" -ne 0 ]; then
  echo RESUME854_BOARD_DONE 7
  exit 7
fi
chmod +x "$out/bench"
sha256sum "$out/bench" > "$out/sha256"
put sha256
if [ ! -f /sys/kernel/debug/scheduler_metrics ]; then
  mkdir -p /sys/kernel/debug
  mount -t debugfs debugfs /sys/kernel/debug
fi
overall=0
number=0
for policy in fifo other other fifo fifo other; do
  number=$((number + 1))
  name="round-$number"
  cat /sys/kernel/debug/scheduler_metrics > "$out/$name-before"
  status=0
  "$out/bench" --policy "$policy" --case sched_yield_handoff > "$out/$name.log" 2>&1 || status=$?
  cat /sys/kernel/debug/scheduler_metrics > "$out/$name-after"
  echo "DIAGNOSTIC_EXIT $status" >> "$out/$name.log"
  put "$name.log"
  put "$name-before"
  put "$name-after"
  if [ "$status" -ne 0 ]; then overall=1; fi
done
echo "RESUME854_BOARD_DONE $overall"
[ "$overall" -eq 0 ]
