#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-x86_64}"
apk_cache="$workspace/target/make-apk-cache/$arch"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

prefetch_make() {
    local apk_arch

    case "$arch" in
        x86_64) apk_arch="x86_64" ;;
        riscv64) apk_arch="riscv64" ;;
        aarch64) apk_arch="aarch64" ;;
        loongarch64) apk_arch="loongarch64" ;;
        *)
            echo "error: unsupported make app arch: $arch" >&2
            exit 1
            ;;
    esac

    mkdir -p "$apk_cache"
    APK_ARCH="$apk_arch" APK_CACHE="$apk_cache" OVERLAY_DIR="$overlay_dir" python3 <<'PY'
import io
import os
import shutil
import tarfile
import urllib.request

apk_arch = os.environ["APK_ARCH"]
apk_cache = os.environ["APK_CACHE"]
overlay_dir = os.environ["OVERLAY_DIR"]
branch = "v3.23"
mirrors = [
    "http://mirrors.huaweicloud.com/alpine",
    "http://mirrors.aliyun.com/alpine",
    "http://mirrors.tuna.tsinghua.edu.cn/alpine",
    "http://mirrors.cernet.edu.cn/alpine",
    "http://dl-cdn.alpinelinux.org/alpine",
]


def fetch_bytes(path):
    last_error = None
    for mirror in mirrors:
        url = f"{mirror}/{branch}/{path}"
        try:
            with urllib.request.urlopen(url, timeout=60) as resp:
                return resp.read(), mirror
        except Exception as exc:
            last_error = exc
            print(f"warning: failed to fetch {url}: {exc}", flush=True)
    raise RuntimeError(f"all mirrors failed for {path}: {last_error}")


def fetch_file(path, target):
    if os.path.exists(target) and os.path.getsize(target) > 0:
        print(f"MAKE_PREFETCH cached {os.path.basename(target)}", flush=True)
        return
    tmp = target + ".tmp"
    data, mirror = fetch_bytes(path)
    with open(tmp, "wb") as out:
        out.write(data)
    os.replace(tmp, target)
    print(f"MAKE_PREFETCH downloaded {os.path.basename(target)} from {mirror}", flush=True)


index_data, _ = fetch_bytes(f"main/{apk_arch}/APKINDEX.tar.gz")
with tarfile.open(fileobj=io.BytesIO(index_data), mode="r:gz") as archive:
    index = archive.extractfile("APKINDEX").read().decode()

version = None
for block in index.strip().split("\n\n"):
    fields = {}
    for line in block.splitlines():
        if len(line) > 2 and line[1] == ":":
            fields.setdefault(line[0], []).append(line[2:])
    if fields.get("P", [None])[0] == "make":
        version = fields.get("V", [None])[0]
        break

if not version:
    raise RuntimeError(f"make package not found for {apk_arch}")

apk_name = f"make-{version}.apk"
apk_path = os.path.join(apk_cache, apk_name)
fetch_file(f"main/{apk_arch}/{apk_name}", apk_path)

make_out = os.path.join(overlay_dir, "usr/bin/make")
os.makedirs(os.path.dirname(make_out), exist_ok=True)
with tarfile.open(apk_path, mode="r:*") as archive:
    member = None
    for candidate in ("usr/bin/make", "./usr/bin/make"):
        try:
            member = archive.getmember(candidate)
            break
        except KeyError:
            pass
    if member is None:
        raise RuntimeError(f"{apk_name} does not contain usr/bin/make")
    source = archive.extractfile(member)
    if source is None:
        raise RuntimeError(f"failed to extract usr/bin/make from {apk_name}")
    with open(make_out, "wb") as out:
        shutil.copyfileobj(source, out)
os.chmod(make_out, 0o755)
print(f"MAKE_PREFETCH installed /usr/bin/make from {apk_name}", flush=True)
PY
}

prefetch_make

install -Dm0755 "$app_dir/sh/make-test.sh" "$overlay_dir/usr/bin/make-test.sh"
install -Dm0644 "$app_dir/project/Makefile" "$overlay_dir/usr/src/make-smoke/Makefile"
install -Dm0644 "$app_dir/project/src/message.txt" "$overlay_dir/usr/src/make-smoke/src/message.txt"
install -Dm0644 "$app_dir/project/src/answer.txt" "$overlay_dir/usr/src/make-smoke/src/answer.txt"
