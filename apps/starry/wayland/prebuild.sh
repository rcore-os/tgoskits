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
import urllib.parse
import urllib.request

apk_arch, branch, cache_dir, guest_cache_dir = sys.argv[1:]
mirrors = [
    "https://mirrors.huaweicloud.com/alpine",
    "https://mirrors.aliyun.com/alpine",
    "https://mirrors.tuna.tsinghua.edu.cn/alpine",
    "https://mirrors.cernet.edu.cn/alpine",
    "https://dl-cdn.alpinelinux.org/alpine",
]
repos = ["main", "community"]
extra_roots = os.environ.get("STARRY_WAYLAND_EXTRA_APKS", "").split()
roots = ["weston", "weston-backend-drm", "weston-shell-desktop", *extra_roots]
installed_names = set(os.environ.get("STARRY_WAYLAND_INSTALLED_PACKAGES", "").split())
write_install_list = os.environ.get("STARRY_WAYLAND_WRITE_INSTALL_LIST") == "1"

# Treat index fields as untrusted path input even over HTTPS. The guest
# verifies the original signed index before using this offline repository.
COMPONENT_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+~-]*")


def package_filename(name, version):
    for field, value in (("package name", name), ("version", version)):
        if not COMPONENT_RE.fullmatch(value):
            raise ValueError(f"invalid APK {field}: {value!r}")
    return f"{name}-{version}.apk"


def cache_path(root, filename):
    root = os.path.abspath(root)
    # commonpath does not resolve "..", so normalize the target lexically
    # before comparing it with the expected, unresolved root; also refuse a
    # symlinked root or target so neither can move the write boundary.
    target = os.path.normpath(os.path.join(root, filename))
    try:
        contained = os.path.commonpath((root, target)) == root
    except ValueError:
        contained = False
    if not contained:
        raise ValueError(f"APK cache target escapes its root: {filename!r}")
    if os.path.islink(root):
        raise ValueError(f"APK cache root is a symlink: {root}")
    if os.path.islink(target):
        raise ValueError(f"APK cache target is a symlink: {target}")
    return target


def log(message, stream=sys.stdout):
    print(message, file=stream, flush=True)


def dep_key(value):
    value = value.strip()
    if not value or value.startswith("!"):
        return None
    return re.split(r"[<>=]", value, maxsplit=1)[0]


class HTTPSOnlyRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError(f"refusing non-HTTPS redirect: {newurl}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


opener = urllib.request.build_opener(HTTPSOnlyRedirectHandler())


def fetch_bytes(path):
    last_error = None
    for mirror in mirrors:
        url = f"{mirror}/{branch}/{path}"
        try:
            with opener.open(url, timeout=120) as resp:
                return resp.read(), mirror
        except Exception as exc:
            last_error = exc
            log(f"warning: failed to fetch {url}: {exc}", sys.stderr)
    raise RuntimeError(f"all mirrors failed for {path}: {last_error}")


def fetch_file(path, cache_dir, filename):
    last_error = None
    target_path = cache_path(cache_dir, filename)
    tmp = cache_path(cache_dir, filename + ".tmp")
    for mirror in mirrors:
        url = f"{mirror}/{branch}/{path}"
        try:
            with opener.open(url, timeout=120) as resp, open(tmp, "wb") as out:
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
    repo_dir = os.path.join(guest_cache_dir, repo, apk_arch)
    os.makedirs(repo_dir, exist_ok=True)
    with open(cache_path(repo_dir, "APKINDEX.tar.gz"), "wb") as out:
        out.write(data)
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
        filename = package_filename(name, version)
        deps = []
        for dep_line in fields.get("D", []):
            deps.extend(filter(None, (dep_key(dep) for dep in dep_line.split())))
        provides = [name]
        for provide_line in fields.get("p", []):
            provides.extend(filter(None, (dep_key(item) for item in provide_line.split())))
        packages[name] = {
            "name": name,
            "version": version,
            "filename": filename,
            "repo": repo,
            "deps": deps,
            "install_if": [
                key for line in fields.get("i", [])
                for dep in line.split() if (key := dep_key(dep))
            ],
        }
        for provide in provides:
            providers.setdefault(provide, name)

resolved = []
seen = set()
queue = list(roots)
while queue:
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
    # The native solver also selects install_if packages. Prefetch a
    # conservative superset; apk still decides exact version constraints.
    available = seen | installed_names
    for candidate in packages.values():
        conditions = candidate["install_if"]
        if candidate["name"] not in seen and conditions and all(
            (dep if dep in packages else providers.get(dep)) in available
            for dep in conditions
        ):
            queue.append(candidate["name"])

os.makedirs(cache_dir, exist_ok=True)
os.makedirs(guest_cache_dir, exist_ok=True)
log(f"WAYLAND_PREFETCH resolved {len(resolved)} apk(s) for {apk_arch}")
for pkg in resolved:
    filename = pkg["filename"]
    rel = f"{pkg['repo']}/{apk_arch}/{filename}"
    cached = cache_path(cache_dir, filename)
    if not os.path.exists(cached) or os.path.getsize(cached) == 0:
        mirror = fetch_file(rel, cache_dir, filename)
        log(f"WAYLAND_PREFETCH downloaded {filename} from {mirror}")
    else:
        log(f"WAYLAND_PREFETCH cached {filename}")
    repo_dir = os.path.join(guest_cache_dir, pkg["repo"], apk_arch)
    shutil.copy2(cached, cache_path(repo_dir, filename))

with open(cache_path(guest_cache_dir, "repositories"), "w", encoding="utf-8") as out:
    for repo in repos:
        out.write(f"/usr/local/wayland-apks/{repo}\n")

if write_install_list:
    install_list = os.path.join(guest_cache_dir, "install.list")
    with open(install_list, "w", encoding="utf-8") as out:
        for pkg in resolved:
            if pkg["name"] not in installed_names:
                out.write(f"{pkg['name']}={pkg['version']}\n")

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
