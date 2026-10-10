#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
out_dir="$repo_root/target/starry-orangepi5plus-selfbuild"
remote_app=/opt/starry-orangepi5plus-selfbuild
remote_rootfs="$remote_app/rootfs"
linux_user="${BOARD_LINUX_USER:-orangepi}"
ssh_port="${BOARD_SSH_PORT:-22}"
host=""

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/orangepi-5-plus-selfbuild/provision_rootfs.sh --host BOARD_IP

Run while the OrangePi 5 Plus is in its default Linux. The command creates or
reuses one Debian 12 arm64 glibc rootfs, installs the pinned Rust toolchain and
offline Cargo cache, and stages the exact current workspace source snapshot.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host)
            host="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

[[ "$host" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid --host" >&2; exit 2; }
[[ "$linux_user" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]] \
    || { echo "invalid BOARD_LINUX_USER" >&2; exit 2; }
if ! [[ "$ssh_port" =~ ^[0-9]+$ ]]; then
    echo "invalid BOARD_SSH_PORT" >&2
    exit 2
fi
if ((10#$ssh_port < 1 || 10#$ssh_port > 65535)); then
    echo "invalid BOARD_SSH_PORT" >&2
    exit 2
fi

for command in git rsync sha256sum ssh tar; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "$command not found" >&2
        exit 1
    }
done

toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$repo_root/rust-toolchain.toml")"
case "$toolchain" in
    ''|*[!A-Za-z0-9._-]*) echo "invalid pinned Rust toolchain: $toolchain" >&2; exit 1 ;;
esac

mkdir -p "$out_dir/tmp"
bundle_dir="$(mktemp -d "$out_dir/tmp/source.XXXXXX")"
cleanup() {
    rm -rf "$bundle_dir"
}
trap cleanup EXIT

file_list="$bundle_dir/files"
source_archive="$bundle_dir/tgoskits-src.tar"
source_meta="$bundle_dir/tgoskits-source.meta"
git -C "$repo_root" ls-files -z --cached --others --exclude-standard > "$file_list"
tar -C "$repo_root" --null --files-from="$file_list" -cf "$source_archive"
source_sha256="$(sha256sum "$source_archive" | awk '{ print $1 }')"
commit="$(git -C "$repo_root" rev-parse HEAD)"
ref="$(git -C "$repo_root" symbolic-ref --quiet --short HEAD || echo detached)"
if [ -n "$(git -C "$repo_root" status --porcelain --untracked-files=all)" ]; then
    dirty=true
else
    dirty=false
fi
cat > "$source_meta" <<EOF
commit=$commit
ref=$ref
dirty=$dirty
source_archive_sha256=$source_sha256
toolchain=$toolchain
created_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

remote="${linux_user}@${host}"
ssh_args=(-p "$ssh_port")
rsync_ssh="ssh -p $ssh_port"
ssh "${ssh_args[@]}" "$remote" sudo -n install -d -m 0755 "$remote_app"
ssh "${ssh_args[@]}" "$remote" sudo -n install -d -m 0755 \
    -o "$linux_user" -g "$linux_user" "$remote_app/incoming"

rsync -a -e "$rsync_ssh" \
    "$app_dir/provision_rootfs_remote.sh" \
    "$app_dir/boot_script_is_starry.sh" \
    "$app_dir/install_source_link.sh" \
    "$app_dir/validate_sha256.sh" \
    "$app_dir/init.sh" \
    "$app_dir/restore_linux_boot.sh" \
    "$app_dir/set_guest_clock.sh" \
    "$source_archive" \
    "$source_meta" \
    "$remote:$remote_app/incoming/"

ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/provision_rootfs_remote.sh" \
    "$remote_app/provision_rootfs_remote.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/boot_script_is_starry.sh" \
    "$remote_app/boot_script_is_starry.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/validate_sha256.sh" \
    "$remote_app/validate_sha256.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/install_source_link.sh" \
    "$remote_app/install_source_link.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/init.sh" "$remote_app/init.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/restore_linux_boot.sh" "$remote_app/restore_linux_boot.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/set_guest_clock.sh" "$remote_app/set_guest_clock.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n \
    "$remote_app/provision_rootfs_remote.sh" \
    "$remote_rootfs" \
    "$toolchain" \
    "$remote_app/incoming/tgoskits-src.tar" \
    "$source_sha256" \
    "$remote_app/incoming/tgoskits-source.meta"
ssh "${ssh_args[@]}" "$remote" sudo -n sync

echo "starry_selfbuild_board=$remote"
echo "starry_selfbuild_rootfs=$remote_rootfs"
echo "starry_selfbuild_source_sha256=$source_sha256"
