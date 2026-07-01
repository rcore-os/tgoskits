#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?error: STARRY_ARCH is required}"
rootfs="${STARRY_ROOTFS:?error: STARRY_ROOTFS is required}"
staging_root="${STARRY_STAGING_ROOT:?error: STARRY_STAGING_ROOT is required}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

case "$arch" in
    aarch64) qemu_runner="qemu-aarch64-static" ;;
    riscv64) qemu_runner="qemu-riscv64-static" ;;
    x86_64) qemu_runner="qemu-x86_64-static" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static" ;;
    *) echo "error: unsupported git app arch: $arch" >&2; exit 1 ;;
esac

for tool in debugfs "$qemu_runner" python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: required host tool not found: $tool" >&2
        exit 1
    }
done

extract_rootfs() {
    rm -rf "$staging_root"
    mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || {
        echo "error: base rootfs has no /sbin/apk" >&2
        exit 1
    }
}

normalize_staging_absolute_symlinks() {
    STAGING_ROOT="$staging_root" python3 <<'PY'
import os

staging_root = os.environ["STAGING_ROOT"]

for root, _, names in os.walk(staging_root):
    for name in names:
        path = os.path.join(root, name)
        if not os.path.islink(path):
            continue
        target = os.readlink(path)
        if not target.startswith("/"):
            continue
        staged_target = os.path.join(staging_root, target.lstrip("/"))
        if not os.path.exists(staged_target) and not os.path.islink(staged_target):
            continue
        relative_target = os.path.relpath(staged_target, os.path.dirname(path))
        os.unlink(path)
        os.symlink(relative_target, path)
PY
}

snapshot_staging() {
    STAGING_ROOT="$staging_root" SNAPSHOT="$1" python3 <<'PY'
import json
import os
import stat

staging_root = os.environ["STAGING_ROOT"]
snapshot = os.environ["SNAPSHOT"]
entries = {}

for root, dirs, files in os.walk(staging_root):
    for name in dirs + files:
        path = os.path.join(root, name)
        rel = os.path.relpath(path, staging_root)
        try:
            st = os.lstat(path)
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(st.st_mode):
            kind = "link"
            extra = os.readlink(path)
        elif stat.S_ISDIR(st.st_mode):
            kind = "dir"
            extra = ""
        elif stat.S_ISREG(st.st_mode):
            kind = "file"
            extra = str(st.st_size)
        else:
            continue
        entries[rel] = [kind, stat.S_IMODE(st.st_mode), extra]

with open(snapshot, "w", encoding="utf-8") as out:
    json.dump(entries, out, sort_keys=True)
PY
}

install_git() {
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    sed -i 's|https://|http://|g' "$staging_root/etc/apk/repositories"
    echo "GIT_PREBUILD installing git into staging rootfs"
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --update-cache \
            --no-progress \
            --no-scripts \
            add git
}

copy_staging_delta_to_overlay() {
    STAGING_ROOT="$staging_root" OVERLAY_DIR="$overlay_dir" SNAPSHOT="$1" python3 <<'PY'
import json
import os
import shutil
import stat

staging_root = os.environ["STAGING_ROOT"]
overlay_dir = os.environ["OVERLAY_DIR"]
snapshot = os.environ["SNAPSHOT"]
skip_paths = {"etc/resolv.conf"}

with open(snapshot, encoding="utf-8") as src:
    before = json.load(src)


def metadata(path):
    st = os.lstat(path)
    if stat.S_ISLNK(st.st_mode):
        return ["link", stat.S_IMODE(st.st_mode), os.readlink(path)]
    if stat.S_ISDIR(st.st_mode):
        return ["dir", stat.S_IMODE(st.st_mode), ""]
    if stat.S_ISREG(st.st_mode):
        return ["file", stat.S_IMODE(st.st_mode), str(st.st_size)]
    return None


def replace_path(src, dst, meta):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if meta[0] == "dir":
        os.makedirs(dst, exist_ok=True)
        os.chmod(dst, meta[1])
        return
    if os.path.lexists(dst):
        if os.path.isdir(dst) and not os.path.islink(dst):
            shutil.rmtree(dst)
        else:
            os.unlink(dst)
    if meta[0] == "link":
        os.symlink(meta[2], dst)
    elif meta[0] == "file":
        shutil.copy2(src, dst, follow_symlinks=False)


changed = 0
for root, dirs, files in os.walk(staging_root):
    for name in dirs + files:
        src_path = os.path.join(root, name)
        rel = os.path.relpath(src_path, staging_root)
        if rel in skip_paths:
            continue
        meta = metadata(src_path)
        if meta is None or before.get(rel) == meta:
            continue
        replace_path(src_path, os.path.join(overlay_dir, rel), meta)
        changed += 1

print(f"GIT_PREBUILD staged {changed} changed file(s)")
PY
}

snapshot_file="$staging_root.before.json"
extract_rootfs
normalize_staging_absolute_symlinks
snapshot_staging "$snapshot_file"
install_git
normalize_staging_absolute_symlinks
copy_staging_delta_to_overlay "$snapshot_file"
install -Dm0755 "$app_dir/git-test.sh" "$overlay_dir/usr/bin/git-test.sh"
