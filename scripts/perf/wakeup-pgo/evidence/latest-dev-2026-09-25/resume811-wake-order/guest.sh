#!/bin/sh
set -eu

sid="$1"
tag="$2"
base="http://192.168.1.2:2999"
out="/tmp/resume811-$tag"
mkdir -p "$out"
curl --retry 15 --retry-delay 2 --retry-connrefused -fsS \
    "$base/share/sessions/$sid/bench-order" -o "$out/bench-order"
chmod +x "$out/bench-order"
sha256sum "$out/bench-order" > "$out/sha256"
curl -fsS -X PUT -H "X-File-Path: resume811-$tag-sha256" \
    --data-binary "@$out/sha256" "$base/api/v1/sessions/$sid/files" >/dev/null

overall=0
for round in 1 2 3; do
    for policy in other fifo; do
        name="$policy-$round.log"
        rc=0
        "$out/bench-order" --policy "$policy" \
            --case thread_futex_same_cpu > "$out/$name" 2>&1 || rc=$?
        printf 'DIAGNOSTIC_EXIT %s\n' "$rc" >> "$out/$name"
        curl -fsS -X PUT -H "X-File-Path: resume811-$tag-$name" \
            --data-binary "@$out/$name" "$base/api/v1/sessions/$sid/files" >/dev/null
        if [ "$rc" -ne 0 ]; then overall=1; fi
    done
done
printf 'RESUME811_STARRY_DONE %s %s\n' "$tag" "$overall"
exit "$overall"
