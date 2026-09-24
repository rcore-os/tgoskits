#!/bin/sh
set -eu

sid="$1"
tag="$2"
base="http://192.168.1.2:2999"
out="/tmp/resume808-$tag"
mkdir -p "$out"

fetch() {
    curl --retry 15 --retry-delay 2 --retry-connrefused -fsS \
        "$base/share/sessions/$sid/$1" -o "$out/$1"
}
put() {
    curl -fsS -X PUT -H "X-File-Path: resume808-$tag-$1" \
        --data-binary "@$out/$1" "$base/api/v1/sessions/$sid/files" >/dev/null
}

fetch bench
fetch fixed-count
chmod +x "$out/bench" "$out/fixed-count"
sha256sum "$out/bench" "$out/fixed-count" > "$out/sha256"
put sha256

overall=0
for policy in other fifo; do
    for round in 1 2 3; do
        name="$policy-$round.log"
        rc=0
        "$out/fixed-count" "$out/bench" --policy "$policy" \
            --case thread_futex_same_cpu > "$out/$name" 2>&1 || rc=$?
        printf 'DIAGNOSTIC_EXIT %s\n' "$rc" >> "$out/$name"
        put "$name"
        if [ "$rc" -ne 0 ]; then overall=1; fi
    done
done
printf 'RESUME808_DONE %s %s\n' "$tag" "$overall"
exit "$overall"
