#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-}"
rootfs="${STARRY_ROOTFS:-}"
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

    if ! command -v python3 >/dev/null 2>&1; then
        echo "warning: python3 not found; skipping host APK prefetch" >&2
        return 0
    fi

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
roots = ["weston", "weston-backend-drm", "weston-shell-desktop", *extra_roots]
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

populate_overlay() {
    mkdir -p "$overlay_dir/usr/bin"
    cp "$app_dir/wayland-test.sh" "$overlay_dir/usr/bin/wayland-test.sh"
    chmod 0755 "$overlay_dir/usr/bin/wayland-test.sh"

    copy_base_text_file_to_overlay /etc/apk/repositories
    copy_base_text_file_to_overlay /etc/resolv.conf
    prefetch_wayland_apks
}

require_env STARRY_ARCH "$arch"
require_env STARRY_ROOTFS "$rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
populate_overlay
