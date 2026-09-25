#!/bin/sh
set -eu
sid="$1"
base=http://192.168.1.2:2999
out=/tmp/resume840
mkdir -p "$out"

put() {
  curl -fsS -X PUT -H "X-File-Path: resume840-$1" \
    --data-binary "@$out/$1" "$base/api/v1/sessions/$sid/files" >/dev/null
}

for name in frozen control forced; do
  downloaded=0
  for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    if curl --connect-timeout 10 --max-time 20 -fsS \
      "$base/share/sessions/$sid/$name" -o "$out/$name"; then
      downloaded=1
      break
    fi
    sleep 2
  done
  if [ "$downloaded" -ne 1 ]; then
    echo RESUME840_DONE 7
    exit 7
  fi
  chmod +x "$out/$name"
done

sha256sum "$out/frozen" "$out/control" "$out/forced" > "$out/sha256"
put sha256

overall=0
run_case() {
  name="$1"
  binary="$2"
  policy="$3"
  status=0
  "$out/$binary" --policy "$policy" --case thread_futex_same_cpu \
    > "$out/$name.log" 2>&1 || status=$?
  echo "DIAGNOSTIC_EXIT $status" >> "$out/$name.log"
  put "$name.log"
  if [ "$status" -ne 0 ]; then overall=1; fi
}

run_case control-fifo-1 control fifo
run_case forced-fifo-1 forced fifo
run_case frozen-fifo-1 frozen fifo
run_case control-other-1 control other
run_case control-other-2 control other
run_case frozen-fifo-2 frozen fifo
run_case forced-fifo-2 forced fifo
run_case control-fifo-2 control fifo

echo "RESUME840_DONE $overall"
[ "$overall" -eq 0 ]
