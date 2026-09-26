#!/bin/sh
set -eu
sid="$1"
base=http://192.168.1.2:2999
out=/tmp/resume851
mkdir -p "$out"

put() {
  curl -fsS -X PUT -H "X-File-Path: resume851-$1" \
    --data-binary "@$out/$1" "$base/api/v1/sessions/$sid/files" >/dev/null
}

downloaded=0
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if curl --connect-timeout 10 --max-time 20 -fsS \
    "$base/share/sessions/$sid/wake-cost" -o "$out/wake-cost"; then
    downloaded=1
    break
  fi
  sleep 2
done
[ "$downloaded" -eq 1 ] || { echo RESUME851_BOARD_DONE 7; exit 7; }
chmod +x "$out/wake-cost"
sha256sum "$out/wake-cost" > "$out/sha256"
put sha256

overall=0
for spec in 1:fifo 2:other 3:other 4:fifo 5:fifo 6:other; do
  round=${spec%%:*}
  mode=${spec#*:}
  status=0
  "$out/wake-cost" "$mode" > "$out/starry-$round.log" 2>&1 || status=$?
  echo "DIAGNOSTIC_EXIT $status" >> "$out/starry-$round.log"
  put "starry-$round.log"
  if [ "$status" -ne 0 ]; then overall=1; fi
done
echo "RESUME851_BOARD_DONE $overall"
[ "$overall" -eq 0 ]
