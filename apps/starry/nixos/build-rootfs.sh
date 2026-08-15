#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-$(cd -- "$app_dir/../../.." && pwd)}"
architecture="${STARRY_ARCH:-x86_64}"
target="${STARRY_TARGET:-x86_64-unknown-none}"
output="${STARRY_ROOTFS:-$workspace/tmp/axbuild/rootfs/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img}"
NIX=(nix --extra-experimental-features "nix-command flakes")

fail() {
    echo "StarryNixOS artifact error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' is unavailable"
}

validate_request() {
    [ "$architecture" = x86_64 ] || fail "unsupported architecture '$architecture'; expected x86_64"
    [ "$target" = x86_64-unknown-none ] || fail "target '$target' does not match x86_64-unknown-none"
}

inspect_disabled_want() {
    local image="$1"
    local toplevel="$2"
    local target="$3"
    local service="$4"
    local want_path inspection

    want_path="$toplevel/etc/systemd/system/$target.wants/$service"
    inspection="$(debugfs -R "stat $want_path" "$image" 2>&1)"
    if ! printf '%s\n' "$inspection" | grep -q 'File not found'; then
        fail "artifact unexpectedly enables '$service' through $target"
    fi
}

inspect_artifact() {
    local image="$1"
    local expected_toplevel="$2"
    local provenance declared_toplevel activation_stat

    [ -f "$image" ] || fail "Nix rootfs output '$image' is missing"
    [ -s "$image" ] || fail "Nix rootfs output '$image' is empty"
    e2fsck -fn "$image" >/dev/null || fail "ext4 validation failed for '$image'"
    provenance="$(debugfs -R "cat /etc/starry-nixos/provenance" "$image" 2>/dev/null)"
    printf '%s\n' "$provenance" | grep -Eq '^architecture=x86_64-linux$' \
        || fail "artifact architecture provenance is not x86_64-linux"
    declared_toplevel="$(printf '%s\n' "$provenance" | sed -n 's/^system=//p')"
    [ "$declared_toplevel" = "$expected_toplevel" ] \
        || fail "artifact system '$declared_toplevel' does not match closure '$expected_toplevel'"
    debugfs -R "cat /init" "$image" 2>/dev/null \
        | grep -Eq '/init$' \
        || fail "artifact /init does not select the generated NixOS initializer"
    debugfs -R "cat /nix/var/nix/profiles/system" "$image" 2>/dev/null \
        | grep -Fqx "$expected_toplevel" \
        || fail "artifact system profile does not select '$expected_toplevel'"
    activation_stat="$(debugfs -R "stat $expected_toplevel/activate" "$image" 2>&1)"
    ! printf '%s\n' "$activation_stat" | grep -q 'File not found' \
        || fail "artifact is missing NixOS activation data for '$expected_toplevel'"
    inspect_disabled_want "$image" "$expected_toplevel" multi-user.target systemd-logind.service
    inspect_disabled_want "$image" "$expected_toplevel" multi-user.target linger-users.service
    inspect_disabled_want "$image" "$expected_toplevel" multi-user.target systemd-user-sessions.service
    inspect_disabled_want "$image" "$expected_toplevel" multi-user.target resolvconf.service
    inspect_disabled_want "$image" "$expected_toplevel" sysinit.target systemd-udevd.service
    inspect_disabled_want "$image" "$expected_toplevel" sysinit.target systemd-udev-trigger.service
    inspect_disabled_want "$image" "$expected_toplevel" sockets.target systemd-udevd-control.socket
    inspect_disabled_want "$image" "$expected_toplevel" sockets.target systemd-udevd-kernel.socket
    debugfs -R "stat /etc/alpine-release" "$image" 2>&1 \
        | grep -q 'File not found' \
        || fail "artifact unexpectedly contains Alpine runtime identity"
    debugfs -R "stat /etc/apk" "$image" 2>&1 \
        | grep -q 'File not found' \
        || fail "artifact unexpectedly contains APK configuration"
    debugfs -R "stat /var/lib/apk" "$image" 2>&1 \
        | grep -q 'File not found' \
        || fail "artifact unexpectedly contains the APK database"
}

atomic_publish_nonempty() {
    local source="$1"
    local destination="$2"
    local destination_dir temporary

    [ -f "$source" ] && [ -s "$source" ] || return 1
    destination_dir="$(dirname -- "$destination")"
    mkdir -p "$destination_dir"
    temporary="$(mktemp --tmpdir="$destination_dir" ".$(basename -- "$destination").XXXXXX")"
    if ! cp --reflink=auto -- "$source" "$temporary"; then
        rm -f -- "$temporary"
        return 1
    fi
    if ! chmod 644 -- "$temporary"; then
        rm -f -- "$temporary"
        return 1
    fi
    mv -f -- "$temporary" "$destination"
}

write_manifest() {
    local destination="$1"
    local toplevel="$2"
    local systemd_version="$3"
    local image_hash="$4"
    local lock_hash

    lock_hash="$(sha256sum "$app_dir/flake.lock" | cut -d ' ' -f 1)"
    cat >"$destination" <<EOF
architecture=x86_64-linux
target=x86_64-unknown-none
flake_lock_sha256=$lock_hash
system=$toplevel
systemd_version=$systemd_version
image_sha256=$image_hash
EOF
}

publish_artifact() {
    local image="$1"
    local toplevel="$2"
    local manifest_tmp image_hash systemd_version

    inspect_artifact "$image" "$toplevel"
    image_hash="$(sha256sum "$image" | cut -d ' ' -f 1)"
    systemd_version="$("${NIX[@]}" eval --raw "path:$app_dir#systemd.version")"
    manifest_tmp="$(mktemp)"
    trap 'rm -f -- "$manifest_tmp"' EXIT
    write_manifest "$manifest_tmp" "$toplevel" "$systemd_version" "$image_hash"
    atomic_publish_nonempty "$image" "$output" \
        || fail "failed to atomically publish '$output'"
    atomic_publish_nonempty "$manifest_tmp" "$output.manifest" \
        || fail "failed to atomically publish '$output.manifest'"
    rm -f -- "$manifest_tmp"
    trap - EXIT
    echo "StarryNixOS rootfs published: $output"
    cat "$output.manifest"
}

manifest_value() {
    local manifest="$1"
    local key="$2"

    sed -n "s/^${key}=//p" "$manifest"
}

reuse_published_artifact() {
    local manifest="$output.manifest"
    local expected_lock_hash declared_lock_hash declared_system declared_image_hash actual_image_hash

    [ -f "$manifest" ] || fail "published manifest '$manifest' is missing"
    expected_lock_hash="$(sha256sum "$app_dir/flake.lock" | cut -d ' ' -f 1)"
    declared_lock_hash="$(manifest_value "$manifest" flake_lock_sha256)"
    [ "$declared_lock_hash" = "$expected_lock_hash" ] \
        || fail "published image does not match the current flake.lock"
    [ "$(manifest_value "$manifest" architecture)" = x86_64-linux ] \
        || fail "published image manifest has the wrong architecture"
    [ "$(manifest_value "$manifest" target)" = x86_64-unknown-none ] \
        || fail "published image manifest has the wrong target"

    declared_system="$(manifest_value "$manifest" system)"
    [ -n "$declared_system" ] || fail "published image manifest has no system closure"
    inspect_artifact "$output" "$declared_system"

    declared_image_hash="$(manifest_value "$manifest" image_sha256)"
    actual_image_hash="$(sha256sum "$output" | cut -d ' ' -f 1)"
    [ "$declared_image_hash" = "$actual_image_hash" ] \
        || fail "published image hash does not match '$manifest'"
    echo "StarryNixOS rootfs reused after manifest validation: $output"
}

run_self_test() {
    local marker_service_path test_dir previous candidate empty manifest expected_lock_hash

    validate_request
    [ -f "$app_dir/flake.lock" ] || fail "flake.lock is missing"
    grep -q '"type": "github"' "$app_dir/flake.lock" || fail "flake.lock does not pin a GitHub input"
    grep -q 'rootfs_preparation' "$app_dir/qemu-x86_64.toml" || fail "QEMU config is not app-owned"
    ! grep -Eq 'alpine|apk' "$app_dir/qemu-x86_64.toml" || fail "QEMU config contains an Alpine dependency"
    grep -Eq '^[[:space:]]*nix\.enable[[:space:]]*=[[:space:]]*false;' "$app_dir/configuration.nix" \
        || fail "Stage-2 must keep the Nix daemon disabled"
    grep -Eq '^[[:space:]]*system\.installer\.channel\.enable[[:space:]]*=[[:space:]]*lib\.mkForce[[:space:]]+false;' "$app_dir/configuration.nix" \
        || fail "Stage-2 must not initialize the installer channel"
    marker_service_path="$(
        sed -n \
            '/^[[:space:]]*systemd\.services\.starry-nixos-marker[[:space:]]*=/,/^[[:space:]]*script[[:space:]]*=/p' \
            "$app_dir/configuration.nix"
    )"
    printf '%s\n' "$marker_service_path" | grep -Eq '^[[:space:]]*pkgs\.hello[[:space:]]*$' \
        || fail "marker service path does not provide pkgs.hello"
    printf '%s\n' "$marker_service_path" | grep -Eq '^[[:space:]]*after[[:space:]]*=[[:space:]]*\[[[:space:]]*"multi-user\.target"[[:space:]]*\];' \
        || fail "marker service does not wait for multi-user.target"

    test_dir="$(mktemp -d)"
    trap 'rm -rf -- "$test_dir"' EXIT
    previous="$test_dir/published.img"
    candidate="$test_dir/candidate.img"
    empty="$test_dir/empty.img"
    manifest="$test_dir/manifest"
    printf 'previous-image' >"$previous"
    : >"$empty"
    if atomic_publish_nonempty "$empty" "$previous"; then
        fail "empty artifact unexpectedly replaced the published image"
    fi
    [ "$(cat "$previous")" = previous-image ] \
        || fail "failed artifact publication did not preserve the previous image"
    printf 'validated-image' >"$candidate"
    atomic_publish_nonempty "$candidate" "$previous" \
        || fail "validated artifact could not be published"
    [ "$(cat "$previous")" = validated-image ] \
        || fail "atomic publication selected the wrong artifact"
    [ "$(stat -c '%a' "$previous")" = 644 ] \
        || fail "published artifact is not readable by CI containers"

    write_manifest "$manifest" /nix/store/test-system 999.test deadbeef
    expected_lock_hash="$(sha256sum "$app_dir/flake.lock" | cut -d ' ' -f 1)"
    grep -Eq "^flake_lock_sha256=$expected_lock_hash$" "$manifest" \
        || fail "manifest does not bind the flake lock identity"
    grep -Eq '^system=/nix/store/test-system$' "$manifest" \
        || fail "manifest does not bind the closure identity"
    grep -Eq '^architecture=x86_64-linux$' "$manifest" \
        || fail "manifest does not bind the x86_64 closure architecture"
    rm -rf -- "$test_dir"
    trap - EXIT
    echo "STARRY_NIXOS_ARTIFACT_SELF_TEST_PASSED"
}

main() {
    if [ "${1:-}" = --self-test ]; then
        run_self_test
        return
    fi

    require_command e2fsck
    require_command debugfs
    require_command grep
    require_command sha256sum
    validate_request

    if [ "${STARRY_NIXOS_REUSE_ROOTFS:-0}" = 1 ]; then
        reuse_published_artifact
        return
    fi

    require_command nix

    local image toplevel
    toplevel="$("${NIX[@]}" build --no-link --print-out-paths "path:$app_dir#system")"
    image="$("${NIX[@]}" build --no-link --print-out-paths "path:$app_dir#rootfs")"
    inspect_artifact "$image" "$toplevel"
    publish_artifact "$image" "$toplevel"
}

main "$@"
