#!/usr/bin/env bash
# prebuild.sh — provision a CPython 3 scientific-computing environment (NumPy / OpenCV /
# pyarrow / SciPy / SymPy) and stage the exact-assertion carpet suite for StarryOS.
#
# Portable, reproducible model (mirrors the merged python-lang / python-net apps): extract the
# base Alpine rootfs to a staging tree, point its apk repositories at the Alpine v3.23 branch,
# `apk add` the scientific package set INTO the staging tree via qemu-user-static (so apk
# RESOLVES THE CURRENT VERSION + the full musl-native .so closure for the TARGET arch — no
# hardcoded drifting apk URLs, no cache-miss-exit), then copy the python3 binary, its shared-
# library closure, the stdlib + site-packages (numpy / cv2 / pyarrow / scipy / sympy and their
# C/C++/Fortran extension .so) and every staged /usr/lib/*.so* the extensions need into the app
# overlay. The only inputs are the registered base rootfs and the Alpine apk repos.
#
# Branch choice: Alpine v3.23 community ships musl-native binaries of all five packages for all
# four target arches (x86_64 / aarch64 / riscv64 / loongarch64) at the proven versions
# (py3-numpy 2.3.5 / py3-opencv 4.12.0 / py3-pyarrow 21.0.0 / py3-scipy 1.16.x; py3-sympy is
# noarch pure-python). v3.23 is python 3.12.x — the same 3.12 series as the base image, so there
# is no musl/ABI drift. Because the WHOLE closure (including musl) is taken from the v3.23
# staging tree, the overlay is self-consistent regardless of the base image's exact branch. If a
# package/arch were ever missing from v3.23 one would fall through to edge (PYSCI_APK_BRANCH=edge),
# but all five are present in v3.23 for all four arches.
#
# numba is intentionally NOT provisioned: Alpine has no py3-numba musl apk and llvmlite / the LLVM
# JIT it needs has no musl distribution. The carpet reports numba as a documented SKIP.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (per-app rootfs image, injected after this
# script), STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
PROG="$app_dir/programs"

# Alpine branch holding the scientific package set (v3.23 = proven versions, python 3.12).
APK_BRANCH="${PYSCI_APK_BRANCH:-v3.23}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
# Target rootfs size: the scientific closure (numpy + opencv + pyarrow + scipy + their C++/
# Fortran .so, incl. OpenBLAS / Arrow C++ / OpenCV libs) is large (hundreds of MB). The harness
# injects the overlay via debugfs WITHOUT resizing, so an undersized fs SILENTLY TRUNCATES the
# big .so files. Grow first (mirrors the java-lang recipe). Disk is cheap; QEMU maps only what it uses.
ROOTFS_SIZE="${PYSCI_ROOTFS_SIZE:-6G}"
# Optional offline apk cache (speed-up only). Network apk add stays the primary, source-of-truth
# path; a missing cache entry is filled from the network, NEVER an exit.
APK_CACHE="${PYSCI_APK_CACHE:-}"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static" ;;
    riscv64)     qemu_runner="qemu-riscv64-static" ;;
    x86_64)      qemu_runner="qemu-x86_64-static" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v debugfs    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs  >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v e2fsck     >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v truncate   >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf    >/dev/null 2>&1 || missing+=(binutils)
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

# Grow the per-app rootfs image so the injected scientific closure fits without truncation.
# Idempotent: truncate only grows, e2fsck/resize2fs are safe to re-run.
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
    # fails to load its libz/libssl/libcrypto closure ("symbol not found"). Rewrite every absolute
    # symlink under the staging lib dirs to a relative target that resolves inside the staging tree.
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"
        [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

install_python() {
    normalize_symlinks
    # apk can resolve hostnames inside qemu-user with the host's DNS config.
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    # Point apk at the chosen Alpine branch (v3.23 main+community) so `apk add` resolves the
    # CURRENT version of each package + its full musl/openblas/Arrow/opencv closure for the
    # target arch (copied into the overlay below). No hardcoded per-file apk URLs.
    printf '%s/%s/main\n%s/%s/community\n' \
        "$ALPINE_CDN" "$APK_BRANCH" "$ALPINE_CDN" "$APK_BRANCH" \
        > "$staging_root/etc/apk/repositories"
    local cache_args=()
    if [[ -n "$APK_CACHE" ]]; then
        mkdir -p "$APK_CACHE"
        cache_args=(--cache-dir "$APK_CACHE")
        echo "prebuild: using offline apk cache $APK_CACHE (network fills any miss)"
    fi
    echo "prebuild: apk add python3 + scientific set ($APK_BRANCH) via $qemu_runner..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            "${cache_args[@]}" \
            --update-cache --no-progress --no-scripts \
            add python3 py3-numpy py3-opencv py3-pyarrow py3-scipy py3-sympy
    # Lenient version gate: the carpet asserts version-stable invariants, so any CPython >= 3.12
    # is acceptable (never an exact patch string).
    local pyver
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    case "$pyver" in
        python3.1[2-9]|python3.2[0-9]) echo "prebuild: provisioned $pyver" ;;
        *) echo "prebuild: need CPython >= 3.12 but got '$pyver'" >&2; exit 3 ;;
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
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    copy_to_overlay /usr/bin/python3 0755
    copy_so_closure /usr/bin/python3
    # lib-dynload C-extension modules carry their own .so deps
    if [[ -d "$staging_root/usr/lib/$pyver/lib-dynload" ]]; then
        for so in "$staging_root/usr/lib/$pyver/lib-dynload"/*.so; do
            [[ -e "$so" ]] && copy_so_closure "/usr/lib/$pyver/lib-dynload/$(basename "$so")"
        done
    fi
    # The scientific apks landed in site-packages of the staging tree; the cp -a below carries
    # numpy / cv2 / pyarrow / scipy / sympy and their bundled .so into the overlay.
    local sp="$staging_root/usr/lib/$pyver/site-packages"
    mkdir -p "$overlay_dir/usr/lib/$pyver"
    cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true

    # site-packages C-extensions (numpy / cv2 / pyarrow / scipy native .so) carry their own .so
    # deps (libopenblas, libgfortran, libopencv_*, libarrow / libparquet / libthrift, liblapack,
    # libstdc++, ...). Pull the full transitive closure so the runtime loader finds every one of
    # them in the overlay /usr/lib.
    if [[ -d "$sp" ]]; then
        while IFS= read -r so; do
            copy_so_closure "/usr/lib/$pyver/site-packages/${so#"$sp"/}"
        done < <(find "$sp" -name '*.so' 2>/dev/null)
    fi

    # Stage the carpet suite + on-target gate harness under /root/pysci (run by run_pysci.py).
    mkdir -p "$overlay_dir/root/pysci"
    local n=0
    for f in "$app_dir"/python/*.py; do
        [[ -f "$f" ]] || continue
        install -Dm0644 "$f" "$overlay_dir/root/pysci/$(basename "$f")"
        n=$((n + 1))
    done
    # Stage the run wrapper (sets musl loader path + thread caps, then execs python3 run_pysci.py).
    install -Dm0755 "$PROG/run-pysci.sh" "$overlay_dir/usr/bin/run-pysci.sh"
    echo "prebuild: staged $n python module(s); $pyver scientific overlay ready for $arch"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
install_python
populate_overlay
