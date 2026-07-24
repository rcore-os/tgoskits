#!/usr/bin/env bash
# prebuild.sh - provision a CPython 3 environment for the py-sci2 carpet (SciPy / SymPy, with an
# opt-in Numba source build) and stage the exact-assertion suite for StarryOS.
#
# Portable, reproducible model (identical to the merged python-lang / python-net / py-sci apps):
# extract the base Alpine rootfs to a staging tree, point its apk repositories at Alpine v3.23
# (main + community), `apk add` the package set INTO the staging tree via qemu-user-static - so
# apk RESOLVES THE CURRENT VERSION plus the full musl-native .so closure for the TARGET arch (no
# hardcoded drifting apk URLs, no cache-miss-exit) - then copy the python3 binary, its shared-
# library closure, the stdlib + site-packages (scipy / sympy and, transitively, numpy / mpmath /
# openblas / libgfortran) and every staged /usr/lib/*.so* the extensions need into the app
# overlay. The only inputs are the registered base rootfs and the Alpine apk repos.
#
# Package set: `py3-scipy py3-sympy`. scipy pulls its musl-native numpy / openblas / libgfortran /
# libquadmath closure as apk dependencies; sympy (noarch pure python) pulls mpmath. All are
# present in Alpine v3.23 main+community for all four target arches (x86_64 / aarch64 / riscv64 /
# loongarch64) - scipy 1.16.x native, sympy 1.14.x noarch. v3.23 is python 3.12.x (same series as
# the base image), so there is no musl/ABI drift; the whole closure (including musl) is taken from
# the v3.23 staging tree, so the overlay is self-consistent regardless of the base image branch.
#
# numba (with @njit MCJIT JIT), pandas, scikit-learn, matplotlib, networkx and statsmodels ride the
# glibc conda stack (PYSCI2_CONDA=1, default on x86_64/aarch64; see provision_conda + README) - musl
# has no py3-numba apk and no musllinux llvmlite, so conda-forge's pre-built glibc binaries are how
# that stack (and the numba wall) is delivered.
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

# The glibc conda sci/ML stack is delivered by default on the two arches conda ships (x86_64 /
# aarch64); riscv64 / loongarch64 have no conda distribution and run the musl scipy/sympy stack only.
# Override with PYSCI2_CONDA=0/1.
case "$arch" in
    x86_64|aarch64) PYSCI2_CONDA="${PYSCI2_CONDA:-1}" ;;
    *)              PYSCI2_CONDA="${PYSCI2_CONDA:-0}" ;;
esac
export PYSCI2_CONDA

APK_BRANCH="${PYSCI2_APK_BRANCH:-v3.23}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
# scipy's closure (numpy + openblas + libgfortran + libquadmath) is a few hundred MB; the harness
# injects the overlay via debugfs WITHOUT resizing, so an undersized fs SILENTLY TRUNCATES the big
# .so files. Grow first (mirrors the java-lang / py-sci recipe). Disk is cheap; QEMU maps only what
# it uses. The conda stack adds a multi-GB glibc overlay, so grow more when it is enabled.
if [[ "${PYSCI2_CONDA:-0}" == "1" ]]; then
    # miniconda + the conda-forge sci/ML stack (numpy/scipy/numba/pandas/sklearn/matplotlib/...)
    # is a multi-GB overlay; the fs must hold it uncompressed or debugfs silently truncates the .so.
    ROOTFS_SIZE="${PYSCI2_ROOTFS_SIZE:-8G}"
else
    ROOTFS_SIZE="${PYSCI2_ROOTFS_SIZE:-3G}"
fi
APK_CACHE="${PYSCI2_APK_CACHE:-}"

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

# Grow the per-app rootfs image so the injected closure fits without truncation. Idempotent:
# truncate only grows, e2fsck/resize2fs are safe to re-run.
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
    # fails to load its libz/libssl/libcrypto closure. Rewrite every absolute symlink under the
    # staging lib dirs to a relative target that resolves inside the staging tree.
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"
        [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

run_in_staging() {  # run a staging binary under qemu-user with the staging tree as its root
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$@"
}

install_python() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    printf '%s/%s/main\n%s/%s/community\n' \
        "$ALPINE_CDN" "$APK_BRANCH" "$ALPINE_CDN" "$APK_BRANCH" \
        > "$staging_root/etc/apk/repositories"
    local cache_args=()
    if [[ -n "$APK_CACHE" ]]; then
        mkdir -p "$APK_CACHE"
        cache_args=(--cache-dir "$APK_CACHE")
        echo "prebuild: using offline apk cache $APK_CACHE (network fills any miss)"
    fi
    echo "prebuild: apk add python3 py3-scipy py3-sympy ($APK_BRANCH) via $qemu_runner..."
    run_in_staging "$staging_root/sbin/apk" \
        --root "$staging_root" \
        --repositories-file "$staging_root/etc/apk/repositories" \
        --keys-dir "$staging_root/etc/apk/keys" \
        "${cache_args[@]}" \
        --update-cache --no-progress --no-scripts \
        add python3 py3-scipy py3-sympy
    # Lenient version gate: the carpet asserts version-stable invariants, so any CPython >= 3.12 is
    # acceptable (never an exact patch string).
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
    # The apks landed in site-packages of the staging tree; the cp -a below carries scipy / sympy /
    # numpy / mpmath (and any opt-in llvmlite / numba) and their bundled .so into the overlay.
    local sp="$staging_root/usr/lib/$pyver/site-packages"
    mkdir -p "$overlay_dir/usr/lib/$pyver"
    cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true

    # site-packages C-extensions (scipy / numpy native .so, plus opt-in llvmlite / numba .so) carry
    # their own deps (libopenblas, libgfortran, libquadmath, libstdc++, libLLVM, ...). Pull the full
    # transitive closure so the runtime loader finds every one of them in the overlay /usr/lib.
    if [[ -d "$sp" ]]; then
        while IFS= read -r so; do
            copy_so_closure "/usr/lib/$pyver/site-packages/${so#"$sp"/}"
        done < <(find "$sp" -name '*.so' 2>/dev/null)
    fi

    # Stage the carpet suite + on-target gate harness under /root/pysci2 (run by run_pysci2.py).
    mkdir -p "$overlay_dir/root/pysci2"
    local n=0
    for f in "$app_dir"/python/*.py; do
        [[ -f "$f" ]] || continue
        install -Dm0644 "$f" "$overlay_dir/root/pysci2/$(basename "$f")"
        n=$((n + 1))
    done
    # Stage the run wrapper (sets musl loader path + thread caps, then execs python3 run_pysci2.py).
    install -Dm0755 "$PROG/run-pysci2.sh" "$overlay_dir/usr/bin/run-pysci2.sh"
    echo "prebuild: staged $n python module(s); $pyver scipy/sympy overlay ready for $arch"
}

# --- conda (glibc, x86_64/aarch64) bring-up: miniconda + conda-forge sci stack ---
# Guarded by PYSCI2_CONDA=1. StarryOS runs glibc user-space (gcompat + libc6 closure), so a
# relocatable Miniforge Python + conda-forge pre-built numba/llvmlite(static LLVM20)/numpy/scipy
# deliver the full CPU sci stack (and break the numba wall) on the two arches conda ships:
# x86_64 and aarch64. riscv64/loongarch64 have no conda distribution (upstream missing arch).
CONDA_ROOT="/opt/miniconda"
# Download/cache dir for the Miniforge installer, conda prefixes and Debian libc debs. Defaults to a
# per-user cache (writable on any build host, persists across builds, never committed); override with
# PYSCI2_CONDA_DL. Everything here is fetched from public URLs, so an empty/cleared cache just re-fetches.
CONDA_DL="${PYSCI2_CONDA_DL:-${XDG_CACHE_HOME:-$HOME/.cache}/py-sci2-conda}"

provision_conda() {
    [[ "${PYSCI2_CONDA:-0}" == 1 ]] || return 0
    # Official SHA256 digests from https://github.com/conda-forge/miniforge/releases/tag/26.3.2-3
    local mf_x86_sha=848194851a98903134187fbb4ab50efe87b003e0c0f808f97644b7524a62bf2c
    local mf_aa_sha=2c113a69297e612b01ca0f320c22a3107a11f2ab9b573d79ac868a175945ce29
    # Download and verify SHA256; trust cached copy only after re-verifying its digest.
    miniforge_fetch() {
        local file="$1" expected="$2"
        local cache="$CONDA_DL/$file"
        if [[ -f "$cache" ]]; then
            local got; got="$(sha256sum "$cache" | cut -d' ' -f1)"
            [[ "$got" == "$expected" ]] && return 0
            echo "prebuild: cached $file SHA256 mismatch (got $got), re-downloading" >&2
            rm -f "$cache"
        fi
        local tmp; tmp="$(mktemp "$CONDA_DL/${file}.XXXXXX")"
        curl -fsSL -o "$tmp" \
            "https://github.com/conda-forge/miniforge/releases/download/26.3.2-3/$file" \
            || { rm -f "$tmp"; echo "prebuild: download failed for $file" >&2; return 1; }
        local got; got="$(sha256sum "$tmp" | cut -d' ' -f1)"
        if [[ "$got" != "$expected" ]]; then
            echo "prebuild: SHA256 mismatch for $file: expected $expected got $got" >&2
            rm -f "$tmp"; return 1
        fi
        mv "$tmp" "$cache"
    }
    local inst inst_sha
    case "$arch" in
        x86_64)  inst=Miniforge3-26.3.2-3-Linux-x86_64.sh;  inst_sha="$mf_x86_sha" ;;
        aarch64) inst=Miniforge3-26.3.2-3-Linux-aarch64.sh; inst_sha="$mf_aa_sha" ;;
        *) echo "prebuild: conda unavailable on $arch (upstream ships no installer); skipping" ; return 0 ;;
    esac
    mkdir -p "$CONDA_DL"
    miniforge_fetch "$inst" "$inst_sha" || return 1
    local cache="$CONDA_DL/$inst"
    local prefix="$staging_root$CONDA_ROOT"
    rm -rf "$prefix"
    local prefix_cache="$CONDA_DL/prefix-$arch"
    if [[ -x "$prefix_cache/bin/python" ]]; then
        # Fast path: a previously-provisioned prefix (installer + conda install) is cached, so copy
        # it verbatim instead of re-solving. The install path below stays the reproducible default.
        echo "prebuild: copying cached conda prefix ($arch) into staging ..."
        mkdir -p "$(dirname "$prefix")"
        cp -a "$prefix_cache" "$prefix"
    elif [[ "$arch" == x86_64 ]]; then
        echo "prebuild: running Miniforge installer (host x86_64) into $prefix ..."
        bash "$cache" -b -p "$prefix" >/dev/null
        echo "prebuild: conda install -c conda-forge full CPU sci+ML stack ..."
        "$prefix/bin/conda" install -y -q -c conda-forge \
            numpy numba llvmlite scipy sympy pandas scikit-learn matplotlib \
            networkx statsmodels numexpr >/dev/null 2>&1
    elif [[ "$arch" == aarch64 ]]; then
        # aarch64 conda: solve/build the prefix on the x86_64 build host with a throwaway host conda
        # (CONDA_SUBDIR=linux-aarch64 + conda create --platform), so no aarch64 emulation is needed -
        # the packages are conda-forge's pre-built linux-aarch64 binaries, which run on Starry, not
        # the host. StarryOS's staged aarch64 glibc closure (stage_conda_glibc) then runs them.
        local hostc="$CONDA_DL/hostconda-x86_64" xinst="Miniforge3-26.3.2-3-Linux-x86_64.sh"
        miniforge_fetch "$xinst" "$mf_x86_sha" || return 1
        local xcache="$CONDA_DL/$xinst"
        [[ -x "$hostc/bin/conda" ]] || bash "$xcache" -b -p "$hostc" >/dev/null
        echo "prebuild: conda create --platform linux-aarch64 (native x86_64 solve) into $prefix ..."
        CONDA_SUBDIR=linux-aarch64 "$hostc/bin/conda" create -y -q -p "$prefix" --platform linux-aarch64 \
            -c conda-forge python=3.13 conda numpy numba llvmlite scipy sympy pandas scikit-learn \
            matplotlib networkx statsmodels numexpr >/dev/null 2>&1
        printf 'subdir: linux-aarch64\n' > "$prefix/.condarc"
    else
        echo "prebuild: conda unavailable on $arch (upstream ships no installer); skipping" ; return 0
    fi
    # Both provisioning paths bake the build-time prefix into every console-script shebang - the
    # x86_64 installer bakes "$staging/opt/miniconda/bin/python", the aarch64 `conda create --platform`
    # bakes the cache prefix path. Rewrite any "#!<prefix>/bin/pythonX.Y" shebang to the on-target
    # /opt/miniconda so conda + entry-point scripts run on Starry regardless of how the prefix was made.
    for d in bin condabin; do
        [[ -d "$prefix/$d" ]] || continue
        for f in "$prefix/$d"/*; do
            [[ -f "$f" && -w "$f" ]] || continue
            [[ "$(head -c2 "$f" 2>/dev/null)" == '#!' ]] || continue
            sed -i "1s|^#![^ ]*/bin/\(python[0-9.]*\)$|#!$CONDA_ROOT/bin/\1|" "$f"
        done
    done
    stage_conda_glibc
    # relocate the Miniforge prefix into the overlay verbatim (installer output is relocatable)
    mkdir -p "$overlay_dir$CONDA_ROOT"
    cp -a "$prefix/." "$overlay_dir$CONDA_ROOT/"
    install -Dm0755 "$PROG/run-conda-smoke.sh" "$overlay_dir/usr/bin/run-conda-smoke.sh" 2>/dev/null || true
    echo "prebuild: conda overlay staged for $arch (probe)"
}

# Stage the Debian trixie libc6 closure so StarryOS can run the glibc Miniforge Python.
stage_conda_glibc() {
    local a ma deb deb_sha binbdeb binbdeb_sha
    case "$arch" in
        x86_64)
            a=amd64; ma=x86_64-linux-gnu
            deb=libc6_2.41-12+deb13u3_amd64.deb
            deb_sha=8ffd13165b9ee3f067e2ee670df718e48c1bdaa18676ac93d1de761dbbb3913c
            binbdeb=libc-bin_2.41-12+deb13u3_amd64.deb
            binbdeb_sha=0105bbe1f317d8992bd73217ea9f3dd63e7f1195841f6aca346c570566628fb8
            ;;
        aarch64)
            a=arm64; ma=aarch64-linux-gnu
            deb=libc6_2.41-12+deb13u3_arm64.deb
            deb_sha=ff529924782d3286181188fc265a6a92e7fe28975fb3a925dc0e05c0ca66e52f
            binbdeb=libc-bin_2.41-12+deb13u3_arm64.deb
            binbdeb_sha=02f366115bea79b87fe26c7dec4dc444f9fdcc76440905d88cc26dbef075511b
            ;;
        *) return 0 ;;
    esac
    # Download with HTTPS and verify SHA256 before accepting into cache.
    deb_fetch() {
        local file="$1" expected="$2" url="$3"
        local cache="$CONDA_DL/glibc/$file"
        if [[ -f "$cache" ]]; then
            local got; got="$(sha256sum "$cache" | cut -d' ' -f1)"
            [[ "$got" == "$expected" ]] && return 0
            echo "prebuild: cached $file SHA256 mismatch (got $got, removing)" >&2
            rm -f "$cache"
        fi
        local tmp; tmp="$(mktemp "$CONDA_DL/glibc/${file}.XXXXXX")"
        curl -fsSL -o "$tmp" "https://deb.debian.org/debian/pool/main/g/glibc/$url" || { rm -f "$tmp"; return 1; }
        local got; got="$(sha256sum "$tmp" | cut -d' ' -f1)"
        if [[ "$got" != "$expected" ]]; then
            echo "prebuild: SHA256 mismatch for $file: expected $expected got $got" >&2
            rm -f "$tmp"; return 1
        fi
        mv "$tmp" "$cache"
    }
    mkdir -p "$CONDA_DL/glibc"
    deb_fetch "$deb" "$deb_sha" "$deb" || { echo "prebuild: failed to fetch $deb" >&2; return 1; }
    local dp="$CONDA_DL/glibc/$deb"
    local t; t="$(mktemp -d)"
    ( cd "$t" && ar x "$dp" && tar xf data.tar.* )
    # Verify the expected Debian multiarch directory was unpacked; fail loudly if missing
    # so a wrong arch deb does not silently produce a partial/empty glibc closure.
    [[ -d "$t/usr/lib/$ma" ]] || {
        echo "prebuild: $deb missing usr/lib/$ma - wrong arch deb or extraction failed" >&2
        rm -rf "$t"; return 1
    }
    # copy the glibc runtime (ld-linux + libc.so.6 + friends) into the overlay multiarch paths
    mkdir -p "$overlay_dir/lib/$ma" "$overlay_dir/usr/lib/$ma"
    cp -a "$t/usr/lib/$ma/." "$overlay_dir/usr/lib/$ma/"
    [[ -d "$t/lib/$ma" ]] && cp -a "$t/lib/$ma/." "$overlay_dir/lib/$ma/"
    [[ -d "$t/lib64" ]] && { mkdir -p "$overlay_dir/lib64"; cp -a "$t/lib64/." "$overlay_dir/lib64/"; }
    # The conda ELFs bake the ELF interpreter path at link time; copy it verbatim.
    local ld ldpath
    case "$arch" in
        x86_64)  ld=ld-linux-x86-64.so.2 ; ldpath="$overlay_dir/lib64/$ld" ;;
        aarch64) ld=ld-linux-aarch64.so.1 ; ldpath="$overlay_dir/lib/$ld" ;;
    esac
    local real; real="$(find "$t" -name "$ld" -type f 2>/dev/null | head -1)"
    [[ -n "$real" ]] && install -Dm0755 "$real" "$ldpath"
    mkdir -p "$overlay_dir/lib"
    for so in libc.so.6 libm.so.6 libpthread.so.0 libdl.so.2 librt.so.1 libresolv.so.2 libutil.so.1; do
        real="$(find "$t" -name "$so" -type f 2>/dev/null | head -1)"
        [[ -n "$real" ]] && install -Dm0755 "$real" "$overlay_dir/lib/$so"
    done
    rm -rf "$t"
    # ldconfig (from libc-bin) is needed by ctypes.util.find_library inside StarryOS.
    # libc-bin runs in the guest, so its SHA256 must be verified just like libc6.
    deb_fetch "$binbdeb" "$binbdeb_sha" "$binbdeb" || { echo "prebuild: failed to fetch/verify $binbdeb" >&2; return 1; }
    local t2; t2="$(mktemp -d)"
    ( cd "$t2" && ar x "$CONDA_DL/glibc/$binbdeb" && tar xf data.tar.* )
    real="$(find "$t2" -name ldconfig -type f 2>/dev/null | head -1)"
    [[ -n "$real" ]] || { echo "prebuild: ldconfig not found in $binbdeb" >&2; rm -rf "$t2"; return 1; }
    install -Dm0755 "$real" "$overlay_dir/sbin/ldconfig"
    rm -rf "$t2"
    mkdir -p "$overlay_dir/etc/ld.so.conf.d"
    printf 'include /etc/ld.so.conf.d/*.conf\n/lib\n/usr/lib\n/lib64\n/usr/lib/%s\n/lib/%s\n' \
        "$ma" "$ma" > "$overlay_dir/etc/ld.so.conf"
    printf '%s/lib\n' "$CONDA_ROOT" > "$overlay_dir/etc/ld.so.conf.d/conda.conf"
    echo "prebuild: staged Debian libc6 ($a/$ma) glibc closure + $ld loader + ldconfig into overlay"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
install_python
provision_conda
populate_overlay
