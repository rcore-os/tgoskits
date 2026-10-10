#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
mkdir -p "$repo_root/tmp"
test_dir="$(mktemp -d -p "$repo_root/tmp" boot-script-identity.XXXXXX)"
cleanup() {
    rm -rf "$test_dir"
}
trap cleanup EXIT

printf "\0U-Boot script\0load mmc 1:1 \${starry_fit}\0bootm /image.fit\0" \
    > "$test_dir/starry.scr"
printf "\0U-Boot script\0load mmc 0:1 \${linux_kernel}\0booti\0" \
    > "$test_dir/linux.scr"

"$app_dir/boot_script_is_starry.sh" "$test_dir/starry.scr" || {
    echo "StarryOS U-Boot script was not recognized" >&2
    exit 1
}
if "$app_dir/boot_script_is_starry.sh" "$test_dir/linux.scr"; then
    echo "Linux U-Boot script was misidentified as StarryOS" >&2
    exit 1
fi
set +e
"$app_dir/boot_script_is_starry.sh" "$test_dir/missing.scr"
missing_rc=$?
set -e
[ "$missing_rc" = 2 ] || {
    echo "missing U-Boot script did not produce an inspection error" >&2
    exit 1
}

echo "orangepi5plus_boot_script_identity=PASS"
