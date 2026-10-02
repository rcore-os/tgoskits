#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
mkdir -p "$repo_root/tmp"
test_dir="$(mktemp -d -p "$repo_root/tmp" linux-recovery.XXXXXX)"

cleanup() {
    find "$test_dir" -mindepth 1 -delete
    rmdir "$test_dir"
}
trap cleanup EXIT

fail() {
    echo "linux recovery test: $*" >&2
    exit 1
}

env_file="$test_dir/armbianEnv.txt"
printf '%s\n' \
    'verbosity=1' \
    'extraargs=cma=256M' \
    'rootfstype=ext4' > "$env_file"

"$app_dir/ensure_linux_fsck.sh" "$env_file"
grep -qx 'extraargs=cma=256M fsckfix' "$env_file" \
    || fail "fsckfix was not added"

first_sha="$(sha256sum "$env_file")"
"$app_dir/ensure_linux_fsck.sh" "$env_file"
second_sha="$(sha256sum "$env_file")"
[ "$first_sha" = "$second_sha" ] \
    || fail "the second recovery preparation changed the file"

printf '%s\n' 'verbosity=1' 'rootfstype=ext4' > "$env_file"
set +e
"$app_dir/ensure_linux_fsck.sh" "$env_file" > "$test_dir/missing.out" 2>&1
missing_rc=$?
set -e
[ "$missing_rc" != 0 ] \
    || fail "a missing extraargs entry was accepted"
grep -q 'extraargs entry is missing' "$test_dir/missing.out" \
    || fail "the missing-extraargs error lacks its cause"

grep -q 'ensure_linux_fsck.sh.* /boot/armbianEnv.txt' \
    "$app_dir/boot_starry_once_remote.sh" \
    || fail "StarryOS selection does not arm Linux fsck first"

echo "orangepi5plus_linux_recovery=PASS"
