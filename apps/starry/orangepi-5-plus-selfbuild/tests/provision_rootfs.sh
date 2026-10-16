#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
mkdir -p "$repo_root/tmp"
test_dir="$(mktemp -d -p "$repo_root/tmp" provision-rootfs-test.XXXXXX)"
cleanup() {
    rm -rf "$test_dir"
}
trap cleanup EXIT

fake_bin="$test_dir/bin"
ssh_log="$test_dir/ssh.log"
mkdir -p "$fake_bin"

cat > "$fake_bin/git" <<'EOF'
#!/bin/bash
case " $* " in
    *" ls-files "*) printf 'rust-toolchain.toml\0' ;;
    *" rev-parse "*) printf '%s\n' 0123456789abcdef0123456789abcdef01234567 ;;
    *" symbolic-ref "*) printf '%s\n' feat/provision-test ;;
    *" status "*) ;;
    *) echo "unexpected git arguments: $*" >&2; exit 1 ;;
esac
EOF

cat > "$fake_bin/ssh" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$PROVISION_TEST_SSH_LOG"
EOF

cat > "$fake_bin/rsync" <<'EOF'
#!/bin/bash
if ! grep -Eq 'install -d .* -o orangepi -g orangepi|install -d .* -g orangepi -o orangepi' \
    "$PROVISION_TEST_SSH_LOG"; then
    echo "incoming directory is not owned by the upload user" >&2
    exit 13
fi
EOF

chmod +x "$fake_bin/git" "$fake_bin/ssh" "$fake_bin/rsync"

PATH="$fake_bin:$PATH" PROVISION_TEST_SSH_LOG="$ssh_log" \
    "$app_dir/provision_rootfs.sh" --host 192.0.2.1

echo "provision_rootfs_existing_directory_owner=PASS"
