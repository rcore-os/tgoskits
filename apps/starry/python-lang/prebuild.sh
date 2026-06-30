#!/usr/bin/env bash
# prebuild.sh — provision a CPython 3.14 (pure interpreter + stdlib) environment
# into the app rootfs and stage the language carpet suite.
#
# Portable model (mirrors the merged pip app): extract the base Alpine rootfs to a
# staging tree, `apk add python3` INTO it via qemu-user-static (so it works for
# every target arch on an x86 build host), then copy the python3 binary, its
# runtime shared-library closure, and the full standard library into the app
# overlay, plus the test modules under /usr/bin. No host-absolute paths, no
# prebuilt images — the only inputs are the registered base rootfs and the app's
# own python/ sources.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

qemu_runner_candidates() {
    case "$arch" in
        aarch64)     printf '%s\n' qemu-aarch64-static qemu-aarch64 ;;
        riscv64)     printf '%s\n' qemu-riscv64-static qemu-riscv64 ;;
        x86_64)      printf '%s\n' qemu-x86_64-static qemu-x86_64 ;;
        loongarch64) printf '%s\n' qemu-loongarch64-static qemu-loongarch64 ;;
        *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
    esac
}

find_qemu_runner() {
    local candidate
    while IFS= read -r candidate; do
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return 0
        fi
    done < <(qemu_runner_candidates)

    echo "prebuild: missing qemu-user runner for arch $arch; tried: $(qemu_runner_candidates | paste -sd ', ' -)" >&2
    exit 1
}

qemu_runner="$(find_qemu_runner)"

ensure_host_tools() {
    local missing=()
    command -v debugfs    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v readelf    >/dev/null 2>&1 || missing+=(binutils)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            echo "prebuild: installing host tools: ${missing[*]}"
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2
            exit 1
        fi
    fi
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

python_version_dir() {
    local dir
    for dir in "$staging_root"/usr/lib/python3.*; do
        [[ -d "$dir" ]] || continue
        basename "$dir"
    done | grep -E '^python3\.[0-9]+$' | sort -V | tail -1
}

install_python() {
    # apk can resolve hostnames inside qemu-user with the host's DNS config.
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    # CPython 3.14 lives on Alpine edge; the stable base repos only carry 3.12.
    # Point apk at edge (main+community) so `apk add python3` resolves 3.14.x and
    # pulls its matching musl/openssl/... closure (copied into the overlay below).
    local edge="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/edge/main\n%s/edge/community\n' "$edge" "$edge" > "$staging_root/etc/apk/repositories"
    echo "prebuild: apk add python3 (CPython 3.14, pure language + stdlib) from Alpine edge via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --update-cache --no-progress --no-scripts \
            add python3
    # Hard version gate: the carpet must run on a real 3.14 interpreter, not an
    # older python that would merely skip the 3.14-gated checks.
    local pyver
    pyver="$(python_version_dir || true)"
    case "$pyver" in
        python3.14|python3.1[5-9]|python3.2[0-9]) echo "prebuild: provisioned $pyver" ;;
        *) echo "prebuild: need CPython >= 3.14 but got '$pyver' (base rootfs repos must point at Alpine edge)" >&2; exit 3 ;;
    esac
}

copy_to_overlay() {  # guest-path mode
    local src="$staging_root$1" dst="$overlay_dir$1"
    [[ -e "$src" ]] || { echo "prebuild: missing $1 after install" >&2; exit 4; }
    [[ -L "$src" ]] && src="$(readlink -f "$src")"
    install -Dm"$2" "$src" "$dst"
}

# recursively copy the shared-library closure of an ELF into the overlay
copy_so_closure() {
    local pending=("$@") seen=" " gp lib d
    while [[ ${#pending[@]} -gt 0 ]]; do
        gp="${pending[0]}"; pending=("${pending[@]:1}")
        [[ "$seen" == *" $gp "* ]] && continue
        seen+="$gp "
        while IFS= read -r lib; do
            for d in lib usr/lib usr/local/lib; do
                if [[ -e "$staging_root/$d/$lib" ]]; then
                    copy_to_overlay "/$d/$lib" 0644
                    pending+=("/$d/$lib")
                    break
                fi
            done
        done < <(readelf -d "$staging_root$gp" 2>/dev/null | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p')
    done
}

populate_overlay() {
    local pyver
    pyver="$(python_version_dir || true)"
    copy_to_overlay /usr/bin/python3 0755
    copy_so_closure /usr/bin/python3
    # lib-dynload C-extension modules carry their own .so deps
    if [[ -d "$staging_root/usr/lib/$pyver/lib-dynload" ]]; then
        for so in "$staging_root/usr/lib/$pyver/lib-dynload"/*.so; do
            [[ -e "$so" ]] && copy_so_closure "/usr/lib/$pyver/lib-dynload/$(basename "$so")"
        done
    fi
    mkdir -p "$overlay_dir/usr/lib/$pyver"
    cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true

    # stage the carpet suite (run later as `python3 /usr/bin/<file>`)
    local n=0
    for f in "$app_dir"/python/*.py; do
        [[ -f "$f" ]] || continue
        install -Dm0644 "$f" "$overlay_dir/usr/bin/$(basename "$f")"
        n=$((n + 1))
    done
    echo "prebuild: staged $n python module(s); $pyver overlay ready for $arch"
}

ensure_host_tools
extract_base_rootfs
install_python
populate_overlay
