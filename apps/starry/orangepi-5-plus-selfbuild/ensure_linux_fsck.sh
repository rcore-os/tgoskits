#!/bin/sh
set -eu

env_file=${1:-/boot/armbianEnv.txt}
new_file="${env_file}.new"

fail() {
    echo "ensure-linux-fsck: $*" >&2
    exit 1
}

[ -f "$env_file" ] || fail "environment file is missing: $env_file"

extraargs="$(sed -n 's/^extraargs=//p' "$env_file")"
case "$extraargs" in
    '') fail "extraargs entry is missing: $env_file" ;;
    *"
"*) fail "multiple extraargs entries found: $env_file" ;;
esac

case " $extraargs " in
    *" fsckfix "*)
        echo "linux_recovery_fsck=already-armed"
        exit 0
        ;;
esac

sed "s/^extraargs=.*/extraargs=${extraargs} fsckfix/" "$env_file" > "$new_file" \
    || fail "cannot create recovery environment: $new_file"
chmod --reference="$env_file" "$new_file" \
    || fail "cannot preserve recovery environment mode: $new_file"
grep -q '^extraargs=.* fsckfix$' "$new_file" \
    || fail "fsckfix was not added: $new_file"

sync "$new_file" || fail "cannot sync recovery environment: $new_file"
mv -f "$new_file" "$env_file" \
    || fail "cannot activate recovery environment: $env_file"
sync "$env_file" || fail "cannot sync active recovery environment: $env_file"
grep -q '^extraargs=.* fsckfix$' "$env_file" \
    || fail "active recovery environment lacks fsckfix: $env_file"

echo "linux_recovery_fsck=armed"
