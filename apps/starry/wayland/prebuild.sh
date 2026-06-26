#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-}"
rootfs="${STARRY_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v python3 >/dev/null 2>&1 || missing+=(python3)
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required host packages: ${missing[*]}" >&2
        exit 1
    fi
}

copy_base_text_file_to_overlay() {
    local guest_path="$1"
    local target="$overlay_dir$guest_path"
    mkdir -p "$(dirname "$target")"
    if ! debugfs -R "cat $guest_path" "$rootfs" >"$target" 2>/dev/null; then
        rm -f "$target"
        return
    fi
    chmod 0644 "$target"
}

qemu_runner_for_arch() {
    case "$arch" in
        aarch64)     echo qemu-aarch64-static ;;
        riscv64)     echo qemu-riscv64-static ;;
        x86_64)      echo qemu-x86_64-static ;;
        loongarch64) echo qemu-loongarch64-static ;;
        *)           return 1 ;;
    esac
}

prefetch_wayland_apks() {
    local apk_arch branch cache_dir guest_cache_dir

    case "$arch" in
        x86_64 | riscv64 | aarch64 | loongarch64)
            apk_arch="$arch"
            ;;
        *)
            echo "warning: unsupported apk arch for Wayland APK prefetch: $arch" >&2
            return 0
            ;;
    esac

    branch="$(sed -n 's#.*/\(v[0-9][0-9.]*\)/main#\1#p' "$overlay_dir/etc/apk/repositories" 2>/dev/null | head -1)"
    if [[ -z "$branch" ]]; then
        branch="v3.23"
    fi

    cache_dir="$workspace/target/wayland-apks/$branch/$apk_arch"
    guest_cache_dir="$overlay_dir/usr/local/wayland-apks"
    mkdir -p "$cache_dir" "$guest_cache_dir"

    python3 - "$apk_arch" "$branch" "$cache_dir" "$guest_cache_dir" <<'PY'
import io
import os
import re
import shutil
import sys
import tarfile
import urllib.request

apk_arch, branch, cache_dir, guest_cache_dir = sys.argv[1:]
mirrors = [
    "http://mirrors.huaweicloud.com/alpine",
    "http://mirrors.aliyun.com/alpine",
    "http://mirrors.tuna.tsinghua.edu.cn/alpine",
    "http://mirrors.cernet.edu.cn/alpine",
    "http://dl-cdn.alpinelinux.org/alpine",
]
repos = ["main", "community"]
extra_roots = os.environ.get("STARRY_WAYLAND_EXTRA_APKS", "").split()
roots = [
    "weston",
    "weston-backend-drm",
    "weston-shell-desktop",
    "gtk4.0-demo",
    "font-dejavu",
    *extra_roots,
]
installed_names = set(os.environ.get("STARRY_WAYLAND_INSTALLED_PACKAGES", "").split())
write_install_list = os.environ.get("STARRY_WAYLAND_WRITE_INSTALL_LIST") == "1"


def log(message, stream=sys.stdout):
    print(message, file=stream, flush=True)


def dep_key(value):
    value = value.strip()
    if not value or value.startswith("!"):
        return None
    return re.split(r"[<>=]", value, maxsplit=1)[0]


def fetch_bytes(path):
    last_error = None
    for mirror in mirrors:
        url = f"{mirror}/{branch}/{path}"
        try:
            with urllib.request.urlopen(url, timeout=120) as resp:
                return resp.read(), mirror
        except Exception as exc:
            last_error = exc
            log(f"warning: failed to fetch {url}: {exc}", sys.stderr)
    raise RuntimeError(f"all mirrors failed for {path}: {last_error}")


def fetch_file(path, target_path):
    last_error = None
    filename = os.path.basename(target_path)
    for mirror in mirrors:
        url = f"{mirror}/{branch}/{path}"
        tmp = target_path + ".tmp"
        try:
            with urllib.request.urlopen(url, timeout=120) as resp, open(tmp, "wb") as out:
                total_header = resp.headers.get("Content-Length")
                total = int(total_header) if total_header and total_header.isdigit() else 0
                downloaded = 0
                next_report = 2 * 1024 * 1024
                log(f"WAYLAND_PREFETCH downloading {filename} from {mirror}")
                while True:
                    chunk = resp.read(1024 * 1024)
                    if not chunk:
                        break
                    out.write(chunk)
                    downloaded += len(chunk)
                    if downloaded >= next_report:
                        if total:
                            log(
                                f"WAYLAND_PREFETCH downloading {filename} "
                                f"{downloaded // (1024 * 1024)}MiB/{total // (1024 * 1024)}MiB"
                            )
                        else:
                            log(
                                f"WAYLAND_PREFETCH downloading {filename} "
                                f"{downloaded // (1024 * 1024)}MiB"
                            )
                        next_report += 2 * 1024 * 1024
            os.replace(tmp, target_path)
            return mirror
        except Exception as exc:
            last_error = exc
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
            log(f"warning: failed to fetch {url}: {exc}", sys.stderr)
    raise RuntimeError(f"all mirrors failed for {path}: {last_error}")


packages = {}
providers = {}
for repo in repos:
    log(f"WAYLAND_PREFETCH fetching index {repo}/{apk_arch}")
    data, _ = fetch_bytes(f"{repo}/{apk_arch}/APKINDEX.tar.gz")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        index = archive.extractfile("APKINDEX").read().decode()
    for block in index.strip().split("\n\n"):
        fields = {}
        for line in block.splitlines():
            if len(line) > 2 and line[1] == ":":
                fields.setdefault(line[0], []).append(line[2:])
        name = fields.get("P", [None])[0]
        version = fields.get("V", [None])[0]
        if not name or not version:
            continue
        deps = []
        for dep_line in fields.get("D", []):
            deps.extend(filter(None, (dep_key(dep) for dep in dep_line.split())))
        provides = [name]
        for provide_line in fields.get("p", []):
            provides.extend(filter(None, (dep_key(item) for item in provide_line.split())))
        packages[name] = {
            "name": name,
            "version": version,
            "repo": repo,
            "deps": deps,
        }
        for provide in provides:
            providers.setdefault(provide, name)

resolved = []
seen = set()
queue = list(roots)
while queue:
    request = queue.pop(0)
    name = request if request in packages else providers.get(request)
    if not name or name in seen:
        continue
    seen.add(name)
    pkg = packages[name]
    resolved.append(pkg)
    for dep in pkg["deps"]:
        dep_name = dep if dep in packages else providers.get(dep)
        if dep_name and dep_name not in seen:
            queue.append(dep_name)

os.makedirs(cache_dir, exist_ok=True)
os.makedirs(guest_cache_dir, exist_ok=True)
log(f"WAYLAND_PREFETCH resolved {len(resolved)} apk(s) for {apk_arch}")
for pkg in resolved:
    filename = f"{pkg['name']}-{pkg['version']}.apk"
    rel = f"{pkg['repo']}/{apk_arch}/{filename}"
    cached = os.path.join(cache_dir, filename)
    if not os.path.exists(cached) or os.path.getsize(cached) == 0:
        mirror = fetch_file(rel, cached)
        log(f"WAYLAND_PREFETCH downloaded {filename} from {mirror}")
    else:
        log(f"WAYLAND_PREFETCH cached {filename}")
    shutil.copy2(cached, os.path.join(guest_cache_dir, filename))

if write_install_list:
    install_list = os.path.join(guest_cache_dir, "install.list")
    with open(install_list, "w", encoding="utf-8") as out:
        for pkg in resolved:
            if pkg["name"] not in installed_names:
                filename = f"{pkg['name']}-{pkg['version']}.apk"
                out.write(f"/usr/local/wayland-apks/{filename}\n")

log(f"WAYLAND_PREFETCH prepared {len(resolved)} apk(s) for {apk_arch}")
PY
}


normalize_staging_absolute_symlinks() {
    STAGING_ROOT="$staging_root" python3 <<PY
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

install_wayland_packages_in_staging() {
    local qemu_runner apk_dir

    qemu_runner="$(qemu_runner_for_arch)" || {
        echo "warning: unsupported arch for Wayland preinstall: $arch" >&2
        return 0
    }
    if ! command -v "$qemu_runner" >/dev/null 2>&1; then
        echo "warning: $qemu_runner not found; leaving prefetched APKs for guest install" >&2
        return 0
    fi

    apk_dir="$overlay_dir/usr/local/wayland-apks"
    if ! compgen -G "$apk_dir/*.apk" >/dev/null; then
        echo "warning: no prefetched Wayland APKs found under $apk_dir" >&2
        return 0
    fi

    echo "WAYLAND_PREBUILD extracting rootfs for qemu-user APK install"
    debugfs -R "rdump / $staging_root" "$rootfs"
    normalize_staging_absolute_symlinks

    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    echo "WAYLAND_PREBUILD installing Weston and GTK demo into staging rootfs"
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --allow-untrusted \
            --no-network \
            --no-progress \
            --no-scripts \
            add "$apk_dir"/*.apk

    for required in \
        "$staging_root/usr/bin/weston" \
        "$staging_root/usr/bin/gtk4-demo"
    do
        if [[ ! -e "$required" ]]; then
            echo "error: APK preinstall did not create ${required/#$staging_root/STARRY_STAGING_ROOT}" >&2
            exit 1
        fi
    done
    if ! compgen -G "$staging_root/usr/lib/libweston-"'*/drm-backend.so' >/dev/null; then
        echo "error: APK preinstall did not create STARRY_STAGING_ROOT/usr/lib/libweston-*/drm-backend.so" >&2
        exit 1
    fi

    STAGING_ROOT="$staging_root" OVERLAY_DIR="$overlay_dir" APK_DIR="$apk_dir" python3 <<'PY'
import os
import shutil
import tarfile

staging_root = os.environ["STAGING_ROOT"]
overlay_dir = os.environ["OVERLAY_DIR"]
apk_dir = os.environ["APK_DIR"]


def guest_to_host(root, guest_path):
    return os.path.join(root, guest_path.lstrip("/"))


def copy_path(guest_path, recursive=False):
    source = guest_to_host(staging_root, guest_path)
    target = guest_to_host(overlay_dir, guest_path)
    if not os.path.lexists(source):
        return
    if os.path.isdir(source) and not os.path.islink(source):
        os.makedirs(target, exist_ok=True)
        shutil.copystat(source, target, follow_symlinks=False)
        if recursive:
            for entry in os.listdir(source):
                copy_path(os.path.join(guest_path.rstrip("/"), entry), recursive=True)
        return
    os.makedirs(os.path.dirname(target), exist_ok=True)
    if os.path.lexists(target):
        if os.path.isdir(target) and not os.path.islink(target):
            shutil.rmtree(target)
        else:
            os.unlink(target)
    if os.path.islink(source):
        os.symlink(os.readlink(source), target)
    else:
        shutil.copy2(source, target, follow_symlinks=False)


def safe_member_path(name):
    name = name.strip("/")
    if not name or name.startswith(".") or "/." in name:
        return None
    parts = name.split("/")
    if any(part in ("", ".", "..") for part in parts):
        return None
    return "/" + name


for filename in sorted(os.listdir(apk_dir)):
    if not filename.endswith(".apk"):
        continue
    with tarfile.open(os.path.join(apk_dir, filename), mode="r:*") as archive:
        for member in archive:
            guest_path = safe_member_path(member.name)
            if guest_path is not None:
                copy_path(guest_path)

for guest_path in (
    "/lib/apk/db",
    "/etc/apk/world",
    "/etc/apk/repositories",
    "/etc/resolv.conf",
):
    copy_path(guest_path, recursive=guest_path == "/lib/apk/db")
PY
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/bin"
    cp "$app_dir/wayland-test.sh" "$overlay_dir/usr/bin/wayland-test.sh"
    chmod 0755 "$overlay_dir/usr/bin/wayland-test.sh"

    copy_base_text_file_to_overlay /etc/apk/repositories
    copy_base_text_file_to_overlay /etc/resolv.conf
    prefetch_wayland_apks
    install_wayland_packages_in_staging
}

require_env STARRY_ARCH "$arch"
require_env STARRY_ROOTFS "$rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
populate_overlay
