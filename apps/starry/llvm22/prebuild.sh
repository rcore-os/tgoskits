#!/usr/bin/env bash
# prebuild.sh - provision the musl-native LLVM 22 toolchain (lli / llc / opt / clang / clang++ /
# lld / llvm-as / llvm-dis / llvm-config / FileCheck / llvm-link / llvm-nm / llvm-objdump /
# llvm-ar) and stage the exact-assertion carpet for StarryOS.
#
# Portable, reproducible model (mirrors the merged python-lang / python-net / py-sci apps):
# extract the base Alpine rootfs into a staging tree, point apk at Alpine edge (llvm22 22.1.x
# lives in edge/main), `apk add` the LLVM 22 tool set INTO the staging tree via
# qemu-user-static (so apk RESOLVES THE CURRENT version + the full musl-native .so closure for
# the TARGET arch - no hardcoded drifting apk URLs, no cache-miss-exit), then copy exactly the
# files owned by the newly installed/upgraded packages (the transaction delta) into the app
# overlay. The base image already ships gcc / binutils / musl-dev (crt + linker) that clang's
# C -> exe path uses; `gcc musl-dev binutils` are still named on the apk line so a base that
# lacks them pulls them into the copied delta. `g++` adds the C++ standard headers + libstdc++
# that clang++'s STL path needs.
#
# Alpine branch: edge/main is the only branch shipping llvm22 (all four target arches:
# x86_64 / aarch64 / riscv64 / loongarch64 at 22.1.x). The whole transaction delta - including
# any upgraded libstdc++ / libncurses - is copied, so the overlay is self-consistent on top of
# the 3.23 base (musl 1.2.5 is unchanged, ABI-compatible).
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (per-app rootfs image, injected after
# this script), STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

APK_BRANCH="${LLVM22_APK_BRANCH:-edge}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
# The LLVM 22 closure (llvm22 + llvm22-libs + clang22 + clang22-libs + deps) is ~760 MiB
# installed. The harness injects the overlay via debugfs WITHOUT resizing, so an undersized fs
# SILENTLY TRUNCATES the big .so files. Grow first (mirrors the py-sci / java-lang recipe).
ROOTFS_SIZE="${LLVM22_ROOTFS_SIZE:-8G}"
APK_CACHE="${LLVM22_APK_CACHE:-}"

PKGS="llvm22 llvm22-dev llvm22-test-utils clang22 lld22 gcc g++ musl-dev binutils"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static" ;;
    riscv64)     qemu_runner="qemu-riscv64-static" ;;
    x86_64)      qemu_runner="qemu-x86_64-static" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v debugfs   >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v truncate  >/dev/null 2>&1 || missing+=(coreutils)
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
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

grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before after
    before=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs $base_rootfs is $((before / 1024 / 1024)) MiB; growing to $ROOTFS_SIZE"
    truncate -s "$ROOTFS_SIZE" "$base_rootfs"
    e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
    resize2fs "$base_rootfs" >/dev/null 2>&1
    after=$(stat -c %s "$base_rootfs")
    echo "prebuild: rootfs grown to $((after / 1024 / 1024)) MiB (fs resized)"
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

normalize_symlinks() {
    # qemu-user resolves ABSOLUTE symlink targets against the HOST root, so an alpine
    # `usr/lib/libz.so.1 -> /usr/lib/libz.so.1.3.2` dangles on a non-alpine build host and apk
    # fails to load its closure. Rewrite absolute symlinks under the staging lib dirs to relative.
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"
        [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

qapk() {
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" --root "$staging_root" "$@"
}

install_llvm() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    printf '%s/%s/main\n%s/%s/community\n' \
        "$ALPINE_CDN" "$APK_BRANCH" "$ALPINE_CDN" "$APK_BRANCH" \
        > "$staging_root/etc/apk/repositories"
    local cache_args=()
    if [[ -n "$APK_CACHE" ]]; then
        mkdir -p "$APK_CACHE"; cache_args=(--cache-dir "$APK_CACHE")
        echo "prebuild: using offline apk cache $APK_CACHE (network fills any miss)"
    fi
    echo "prebuild: apk add [$PKGS] ($APK_BRANCH) via $qemu_runner ..."
    local apk_log="$staging_root/.llvm22-apk.log"
    qapk --repositories-file "$staging_root/etc/apk/repositories" \
         --keys-dir "$staging_root/etc/apk/keys" \
         "${cache_args[@]}" \
         --update-cache --no-progress --no-scripts \
         add $PKGS 2>&1 | tee "$apk_log"

    # Transaction delta = every package installed OR upgraded in this run.
    txn_pkgs=$(grep -oE '(Installing|Upgrading) [a-z0-9._+-]+' "$apk_log" | awk '{print $2}' | sort -u)
    [[ -n "$txn_pkgs" ]] || { echo "prebuild: empty apk transaction" >&2; exit 3; }
    echo "prebuild: transaction delta = $(echo "$txn_pkgs" | wc -w) package(s)"

    # Strict version gate: llvm-config must report major 22.
    local llvm_ver major
    llvm_ver=$(qapk info --description llvm22 2>/dev/null | head -1 || true)
    major=$(QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/lib/llvm22/bin/llvm-config" --version 2>/dev/null | cut -d. -f1)
    case "$major" in
        22) echo "prebuild: provisioned LLVM $(QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" "$qemu_runner" -L "$staging_root" "$staging_root/usr/lib/llvm22/bin/llvm-config" --version 2>/dev/null)" ;;
        *)  echo "prebuild: need LLVM major 22 but llvm-config reported '$major'" >&2; exit 3 ;;
    esac
    [[ -x "$staging_root/usr/lib/llvm22/bin/lli" ]] || { echo "prebuild: lli missing after install" >&2; exit 3; }
    [[ -e "$staging_root/usr/bin/clang"          ]] || { echo "prebuild: clang missing after install" >&2; exit 3; }
}

populate_overlay() {
    # Copy exactly the files owned by the transaction-delta packages (regular files + symlinks,
    # recorded as R: entries under F: folders in apk's installed db) into the overlay, preserving
    # modes + symlinks. tar streams the explicit file list (fast, parents auto-created).
    local filelist="$staging_root/.llvm22-files.txt"
    awk -v pkgs="$txn_pkgs" '
        BEGIN { n = split(pkgs, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") want[a[i]] = 1 }
        /^P:/ { cur = substr($0, 3); inpkg = (cur in want) ? 1 : 0; dir = "" }
        /^F:/ { if (inpkg) dir = substr($0, 3) }
        /^R:/ { if (inpkg) { f = substr($0, 3); print (dir == "") ? f : dir "/" f } }
    ' "$staging_root/lib/apk/db/installed" | sort -u > "$filelist"
    local n; n=$(wc -l < "$filelist")
    [[ "$n" -gt 0 ]] || { echo "prebuild: no files resolved for delta packages" >&2; exit 4; }
    echo "prebuild: copying $n LLVM toolchain file(s) into overlay"
    mkdir -p "$overlay_dir"
    tar -C "$staging_root" -cf - --no-recursion -T "$filelist" | tar -C "$overlay_dir" -xf -

    # Stage the carpet fixtures + on-target launcher.
    mkdir -p "$overlay_dir/root/llvm22/src"
    local c=0
    for f in "$app_dir"/src/*; do
        [[ -f "$f" ]] || continue
        install -Dm0644 "$f" "$overlay_dir/root/llvm22/src/$(basename "$f")"
        c=$((c + 1))
    done
    install -Dm0755 "$app_dir/programs/run-llvm22.sh" "$overlay_dir/usr/bin/run-llvm22.sh"
    echo "prebuild: staged $c carpet fixture(s); LLVM 22 overlay ready for $arch"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
install_llvm
populate_overlay
