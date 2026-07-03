#!/usr/bin/env bash
# prebuild.sh — provision a musl-native CPython 3 runtime and stage the Python TUI carpet
# (textual + casca + rich) for StarryOS.
#
# Reproducible, fetch-at-build model (mirrors the merged py-sci / node-lib apps): extract the base
# Alpine rootfs to a staging tree, point its apk repositories at the Alpine v3.23 branch, `apk add
# python3` INTO the staging tree via qemu-user-static (apk RESOLVES THE CURRENT python3 + its full
# musl .so closure for the TARGET arch — no hard-coded drifting apk URLs, no cache-miss-exit), then
# copy the interpreter, its shared-library closure and the stdlib into the app overlay.
#
# The TUI dependency closure (textual / casca / rich + their pure-Python transitive deps) is NOT
# vendored in the source tree. prebuild runs `pip install --require-hashes --no-deps` from the
# committed, hash-locked assets/requirements.txt to fetch a pinned, integrity-checked site-packages
# into a scratch dir, then copies it into the overlay at /opt/pytui (put FIRST on PYTHONPATH by the
# run wrapper). Every pinned wheel is py3-none-any (pure Python), so the tree is architecture-
# independent and valid for all four target arches; the host's own pip therefore produces a tree
# usable on every target (fallback: the musl pip apk-provisioned into the staging rootfs, run under
# qemu-user-static). The only committed inputs are the base rootfs and the app's assets/ (the pinned
# manifest) + python/ sources. NO wheels or binaries live in the repository.
#
# pip reaches PyPI over the ambient HTTP(S)_PROXY env (set by the app runner). An optional local
# wheel cache (PYTUI_WHEEL_CACHE, or <repo>/download/python-tui/wheels) is added with --find-links
# purely as a speed-up: pip only consumes cache wheels whose sha256 matches the pinned hash and
# fetches every miss from PyPI — a cache miss is NEVER an exit, and the network stays the source of
# truth. If a build host has no network for apk, python install falls back to a documented,
# pre-fetched apk cache at $PYTUI_APK_CACHE/<arch>/ (OPTIONAL).
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (per-app rootfs image, injected after this
# script), STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
ASSETS="$app_dir/assets"
PROG="$app_dir/programs"

# Alpine branch holding python3 (v3.23 = python 3.12, same series as the base image; no musl drift).
APK_BRANCH="${PYTUI_APK_BRANCH:-v3.23}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
# GROW-ONLY target size: the harness injects the overlay via debugfs WITHOUT resizing, so an
# undersized fs SILENTLY TRUNCATES files. CPython + stdlib + the pure-Python site-packages plus the
# heavy posting overlay (musl-native pydantic-core/watchfiles/brotli .so + the tree-sitter closure)
# fit comfortably in 3G; grow only if the image is currently smaller.
ROOTFS_SIZE_MIB="${PYTUI_ROOTFS_SIZE_MIB:-3072}"
# Optional offline apk cache (speed-up only). Network apk add stays the primary, source-of-truth path.
default_apk_cache="$(cd "$app_dir/../../.." 2>/dev/null && pwd)/download/python-tui/apks"
apk_cache="${PYTUI_APK_CACHE:-$default_apk_cache}"
# Optional local wheel cache (speed-up only; hash-filtered, network fills any miss).
default_wheel_cache="$(cd "$app_dir/../../.." 2>/dev/null && pwd)/download/python-tui/wheels"
wheel_cache="${PYTUI_WHEEL_CACHE:-$default_wheel_cache}"

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
    command -v readelf   >/dev/null 2>&1 || missing+=(binutils)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            echo "prebuild: installing host tools: ${missing[*]}"
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

# Grow the per-app rootfs image so the injected python closure fits without truncation.
# GROW-ONLY: only truncate up when the image is smaller than the target, and never swallow a
# resize2fs failure. Idempotent (truncate only grows, e2fsck/resize2fs are safe to re-run).
grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before target
    before=$(stat -c %s "$base_rootfs")
    target=$(( ROOTFS_SIZE_MIB * 1024 * 1024 ))
    if [[ "$before" -lt "$target" ]]; then
        echo "prebuild: growing rootfs from $((before/1024/1024)) MiB to $ROOTFS_SIZE_MIB MiB"
        truncate -s "${ROOTFS_SIZE_MIB}M" "$base_rootfs"
        e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
        resize2fs "$base_rootfs"   # NOT swallowed: a resize failure must surface
    else
        echo "prebuild: rootfs already $((before/1024/1024)) MiB (>= ${ROOTFS_SIZE_MIB} MiB); not shrinking"
    fi
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
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

install_python() {
    normalize_symlinks
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    printf '%s/%s/main\n%s/%s/community\n' \
        "$ALPINE_CDN" "$APK_BRANCH" "$ALPINE_CDN" "$APK_BRANCH" \
        > "$staging_root/etc/apk/repositories"
    echo "prebuild: apk add python3 py3-pip ($APK_BRANCH) via $qemu_runner..."
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" --keys-dir "$staging_root/etc/apk/keys" \
            --update-cache --no-progress --no-scripts add python3 py3-pip
    then echo "prebuild: provisioned python3 via network apk"
    else echo "prebuild: network apk failed; falling back to offline apk cache $apk_cache/$arch"; install_python_offline; fi

    # Lenient version gate: the carpets assert version-stable invariants, so any CPython >= 3.12 is
    # acceptable (never an exact patch string).
    local pyver
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    case "$pyver" in
        python3.1[2-9]|python3.2[0-9]) echo "prebuild: provisioned $pyver" ;;
        *) echo "prebuild: need CPython >= 3.12 but got '$pyver'" >&2; exit 3 ;;
    esac
}

install_python_offline() {
    local dir="$apk_cache/$arch"
    [[ -d "$dir" ]] || { echo "prebuild: offline apk cache missing: $dir" >&2; exit 3; }
    local apks=( musl libbz2 libffi "libgcc" gdbm "libstdc++" mpdecimal ncurses-libs readline
        sqlite-libs xz-libs zlib libcrypto3 libssl3 python3 py3-pip )
    local pkg f
    for pkg in "${apks[@]}"; do
        f="$(ls "$dir/${pkg}"-[0-9]*.apk 2>/dev/null | head -1 || true)"
        [[ -n "$f" ]] || continue   # best-effort: only python3 itself is strictly required below
        tar -xzf "$f" -C "$staging_root" --exclude='.PKGINFO' --exclude='.SIGN.*' \
            --exclude='.pre-install' --exclude='.post-install' --exclude='.trigger' 2>/dev/null || true
    done
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: offline apk cache produced no python3" >&2; exit 3; }
    echo "prebuild: provisioned python3 offline from apk cache"
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
                    copy_to_overlay "/$d/$lib" 0644; pending+=("/$d/$lib"); break
                fi
            done
        done < <(readelf -d "$staging_root$gp" 2>/dev/null | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p')
    done
}

# Fetch the pinned, hash-locked textual/casca/rich closure into $1 with pip. Nothing binary lives
# in the source tree; the wheels are pure-Python (py3-none-any) hence architecture-independent.
provision_site_packages() {
    local dest="$1" reqfile="$2"; shift 2
    local sanity=("$@")
    [[ -f "$reqfile" ]] || { echo "prebuild: missing $reqfile" >&2; exit 7; }
    rm -rf "$dest"; mkdir -p "$dest"

    local findlinks=()
    if [[ -d "$wheel_cache" ]]; then
        echo "prebuild: adding local wheel cache $wheel_cache (--find-links; hash-filtered, network fills misses)"
        findlinks=(--find-links "$wheel_cache")
    fi

    # Primary: the build host's own pip (pure-Python wheels -> arch-independent tree).
    if command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; then
        echo "prebuild: pip install (require-hashes, no-deps) via host pip $(python3 -m pip --version | awk '{print $2}')"
        if python3 -m pip install --require-hashes --no-deps --no-cache-dir \
            "${findlinks[@]}" -r "$reqfile" --target "$dest"; then
            :
        else
            echo "prebuild: host pip failed; trying musl pip under qemu-user" >&2
            provision_site_packages_qemu "$dest" "$reqfile"
        fi
    else
        echo "prebuild: no usable host pip; using musl pip under qemu-user"
        provision_site_packages_qemu "$dest" "$reqfile"
    fi

    # Sanity: every top-level package the carpet imports must be present.
    local m
    for m in "${sanity[@]}"; do
        [[ -e "$dest/$m" || -e "$dest/${m}.py" ]] || {
            echo "prebuild: required package '$m' missing after pip install" >&2; exit 7; }
    done
    echo "prebuild: staged site-packages ($(du -sh "$dest" | cut -f1)) into overlay $dest"
}

# Fallback: run the staging rootfs's musl pip under qemu-user-static (same-arch V8-style JIT is not
# a concern for CPython; pure-Python wheels install without a compiler).
provision_site_packages_qemu() {
    local dest="$1" reqfile="$2" pip_main="$staging_root/usr/bin/pip3"
    [[ -x "$staging_root/usr/bin/python3" ]] || { echo "prebuild: no staging python3 for qemu pip" >&2; exit 7; }
    local findlinks=()
    [[ -d "$wheel_cache" ]] && findlinks=(--find-links "$wheel_cache")
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -m pip install \
            --require-hashes --no-deps --no-cache-dir "${findlinks[@]}" \
            -r "$reqfile" --target "$dest"
}

# apk-add extra packages into the SAME staging tree (repos + cache already set by install_python).
apk_add_staging() {
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" --keys-dir "$staging_root/etc/apk/keys" \
            --no-progress --no-scripts add "$@"
}

# leg B (heavy): posting's C-extension deps, provisioned musl-native from Alpine v3.23 apk into the
# staging tree (prebuilt for ALL FOUR arches — zero cross-build), then copied into the posting target
# with their shared-library closure. pydantic + pydantic-core are apk'd together as a MATCHED PAIR
# (a pure pydantic wheel pins one exact core version, so both must come from the same source);
# watchfiles/brotli/pyyaml are the remaining Rust/C deps Alpine ships musl-native per arch.
provision_posting_cext() {
    local dest="$1" pyver sp so pkg
    echo "prebuild: apk add posting C-ext (py3-pydantic{,-core} py3-watchfiles py3-brotli py3-yaml) via $qemu_runner..."
    apk_add_staging py3-pydantic py3-pydantic-core py3-watchfiles py3-brotli py3-yaml \
        || { echo "prebuild: apk add posting C-ext failed" >&2; exit 8; }
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"
    sp="$staging_root/usr/lib/$pyver/site-packages"
    # copy the packages (pydantic pure + pydantic_core/watchfiles/brotli/yaml + top-level *.so) into dest
    for pkg in pydantic pydantic_core watchfiles brotli yaml; do
        [[ -e "$sp/$pkg" ]]    && cp -a "$sp/$pkg"    "$dest/"
        [[ -e "$sp/$pkg.py" ]] && cp -a "$sp/$pkg.py" "$dest/"
    done
    # copy the .dist-info metadata dirs too: pydantic reads importlib.metadata.version("pydantic-core")
    # at import time, so the package files alone are not enough. Distribution names capitalise Brotli/PyYAML.
    for distname in pydantic pydantic_core watchfiles Brotli PyYAML; do
        for di in "$sp/$distname"-*.dist-info; do
            [[ -e "$di" ]] && cp -a "$di" "$dest/"
        done
    done
    for so in "$sp"/_yaml*.so "$sp"/_brotli*.so "$sp"/brotli*.so; do
        [[ -e "$so" ]] && cp -a "$so" "$dest/"
    done
    # pull the transitive .so closure (libgcc_s, libstdc++, libyaml, libbrotli*, ...) into overlay /usr/lib
    while IFS= read -r so; do
        copy_so_closure "/usr/lib/$pyver/site-packages/${so#"$sp"/}"
    done < <(find "$sp" -name '*.so' 2>/dev/null)
    [[ -d "$dest/pydantic_core" ]] || { echo "prebuild: pydantic_core missing after apk provision" >&2; exit 8; }
    echo "prebuild: staged posting C-ext (pydantic{,-core}/watchfiles/brotli/pyyaml) into $dest"
}

# leg B (heavy): tree-sitter core + 15 grammars for textual[syntax] TextArea highlighting. NOT in
# Alpine. Provisioned with the staging musl pip under qemu-user-static: pip takes a matching musllinux
# wheel where PyPI has one (x86_64 all; aarch64 most) and BUILDS the misses from the pinned sdists
# (aarch64 remainder; riscv64/loongarch64 all). Each grammar's own PEP-517 backend handles scanners
# and the multi-parser markdown/xml packages correctly; the build toolchain is apk'd for the non-x86
# arches that need it. Versions are pinned (no drifting URLs); wheels stay arch-specific so no single
# sha256 is possible here — the carpet asserts tree-sitter==0.26.0 which pip enforces via the pins.
provision_posting_treesitter() {
    local dest="$1"
    local ts=(tree-sitter==0.26.0 tree-sitter-bash==0.25.1 tree-sitter-css==0.25.0
        tree-sitter-go==0.25.0 tree-sitter-html==0.23.2 tree-sitter-java==0.23.5
        tree-sitter-javascript==0.25.0 tree-sitter-json==0.24.8 tree-sitter-markdown==0.5.1
        tree-sitter-python==0.25.0 tree-sitter-regex==0.25.0 tree-sitter-rust==0.23.2
        tree-sitter-sql==0.3.7 tree-sitter-toml==0.7.0 tree-sitter-xml==0.7.0
        tree-sitter-yaml==0.7.2)
    if [[ "$arch" != "x86_64" ]]; then
        echo "prebuild: apk add build toolchain (gcc musl-dev python3-dev) for tree-sitter source build on $arch..."
        apk_add_staging gcc musl-dev python3-dev \
            || { echo "prebuild: apk add tree-sitter toolchain failed" >&2; exit 9; }
    fi
    echo "prebuild: pip install tree-sitter core + 15 grammars into $dest (arch=$arch)..."
    if [[ "$arch" == "x86_64" ]]; then
        QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
            "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/python3" -m pip install \
                --no-deps --no-cache-dir "${ts[@]}" --target "$dest" \
            || { echo "prebuild: tree-sitter provisioning failed on $arch" >&2; exit 9; }
    else
        # packaging's musllinux probe EXECs /lib/ld-musl-$arch.so.1 to read the musl version; qemu-user
        # -L only redirects the ELF interpreter, not that exec, so under bare `qemu -L` it resolves to
        # the (nonexistent) host loader. Run pip inside a rootless user-namespace chroot of the staging
        # root, where /lib IS the target-arch loader; binfmt_misc (F-flag) runs the foreign python under
        # qemu with no sudo. Install to a staging-internal path, then copy into the overlay.
        command -v unshare >/dev/null 2>&1 \
            || { echo "prebuild: unshare (util-linux) required for cross-arch tree-sitter build" >&2; exit 9; }
        [[ -e "/proc/sys/fs/binfmt_misc/qemu-${arch}" ]] \
            || { echo "prebuild: binfmt_misc handler qemu-${arch} not registered — install qemu-user-static and register it with the F (fix_binary) flag" >&2; exit 9; }
        # Grammar sdists ship src/parser.c (+ scanner.c for external-scanner grammars) but OMIT their
        # vendored src/tree_sitter/*.h (parser.h, plus alloc.h/array.h scanners include) — a packaging
        # bug. The header set is version-specific (e.g. TSFieldMapSlice was renamed TSMapSlice at
        # tree-sitter 0.25 / parser ABI 15). x86 sidesteps all this via prebuilt musllinux wheels;
        # source-built arches must supply each grammar's exact headers, fetched from its pinned git tag.
        local cdest="/tmp/ts-target"
        rm -rf "$staging_root$cdest"; mkdir -p "$staging_root$cdest"
        # No CA bundle in the staging rootfs (and the build host may front PyPI with a TLS-intercepting
        # proxy); the version pins are the integrity anchor, so trust the PyPI hosts explicitly.
        local TH="--trusted-host pypi.org --trusted-host files.pythonhosted.org --trusted-host pypi.python.org"
        unshare -r chroot "$staging_root" /usr/bin/python3 -m pip install --no-deps --no-cache-dir $TH \
            "${ts[0]}" --target "$cdest" \
            || { echo "prebuild: tree-sitter core provisioning failed on $arch" >&2; exit 9; }
        # Two parser-ABI header fallbacks (TSFieldMapSlice was renamed TSMapSlice at tree-sitter 0.25 /
        # parser ABI 15). Grammars whose git archive omits the vendored src/tree_sitter/parser.h (their
        # parser.c is CLI-generated and not committed) fall back to whichever of these compiles.
        rm -rf "$staging_root/tmp/h14" "$staging_root/tmp/h15"
        mkdir -p "$staging_root/tmp/h14/tree_sitter" "$staging_root/tmp/h15/tree_sitter"
        for h in parser.h alloc.h array.h; do
            curl -fsSL "https://raw.githubusercontent.com/tree-sitter/tree-sitter-json/v0.24.8/src/tree_sitter/$h" -o "$staging_root/tmp/h14/tree_sitter/$h" 2>/dev/null || true
            curl -fsSL "https://raw.githubusercontent.com/tree-sitter/tree-sitter-python/v0.25.0/src/tree_sitter/$h" -o "$staging_root/tmp/h15/tree_sitter/$h" 2>/dev/null || true
        done
        local g name ver repo tag inc built gdir pmeta
        for g in "${ts[@]:1}"; do
            name="${g#tree-sitter-}"; name="${name%%==*}"; ver="${g##*==}"
            # 1) Prefer a prebuilt musllinux wheel (fast, no build).
            if unshare -r chroot "$staging_root" /bin/sh -c "exec /usr/bin/python3 -m pip install --only-binary :all: --no-deps --no-cache-dir $TH '$g' --target $cdest" 2>/dev/null; then
                continue
            fi
            # 2) No wheel: fetch the grammar's complete git source. Repos live under varied orgs and the
            # PyPI homepage is sometimes stale (e.g. tree-sitter-xml moved to the tree-sitter-grammars org),
            # so try the PyPI-declared repo plus the two common orgs, tag v<ver> then <ver>.
            pmeta=$(curl -fsSL "https://pypi.org/pypi/tree-sitter-$name/$ver/json" 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); u=d['info'].get('project_urls') or {}; c=[v for k,v in u.items() if 'github.com' in (v or '')]+([d['info'].get('home_page')] if 'github' in (d['info'].get('home_page') or '') else []); print(next((x for x in c if x),''))" 2>/dev/null)
            pmeta="${pmeta%.git}"
            rm -rf "$staging_root/tmp/gsrc"; mkdir -p "$staging_root/tmp/gsrc"; gdir=""
            for repo in "$pmeta" "https://github.com/tree-sitter-grammars/tree-sitter-$name" "https://github.com/tree-sitter/tree-sitter-$name"; do
                [[ -n "$repo" ]] || continue
                for tag in "v$ver" "$ver"; do
                    if curl -fsSL "$repo/archive/refs/tags/$tag.tar.gz" -o "$staging_root/tmp/gsrc/g.tar.gz" 2>/dev/null \
                        && tar xzf "$staging_root/tmp/gsrc/g.tar.gz" -C "$staging_root/tmp/gsrc" 2>/dev/null; then
                        gdir=$(ls "$staging_root/tmp/gsrc" 2>/dev/null | grep -iE "^tree.?sitter.?$name" | head -1)
                        [[ -n "$gdir" ]] && break 2
                    fi
                done
            done
            # 3) Build attempts: (a) the git archive directory — complete committed source incl split-parser
            # common/ and CLI-generated parser.c where committed (html/rust/xml); (b) the sdist with the
            # grammar's own headers via CPATH; (c) the ABI-14 / ABI-15 tree-sitter parser.h fallbacks (for
            # sdists whose git archive omits the CLI-generated parser.c, e.g. sql).
            built=0
            if [[ -n "$gdir" ]] && unshare -r chroot "$staging_root" /bin/sh -c "exec /usr/bin/python3 -m pip install --no-deps --no-cache-dir $TH /tmp/gsrc/$gdir --target $cdest" 2>/dev/null; then
                built=1
            fi
            if [[ $built == 0 ]]; then
                for inc in "/tmp/gsrc/$gdir/src" /tmp/h14 /tmp/h15; do
                    [[ -n "$gdir" || "$inc" != "/tmp/gsrc/"* ]] || continue
                    if unshare -r chroot "$staging_root" /bin/sh -c "CPATH=$inc exec /usr/bin/python3 -m pip install --no-deps --no-cache-dir $TH '$g' --target $cdest" 2>/dev/null; then
                        built=1; break
                    fi
                done
            fi
            [[ $built == 1 ]] || { echo "prebuild: grammar $g failed to build on $arch" >&2; exit 9; }
        done
        rm -rf "$staging_root/tmp/gsrc" "$staging_root/tmp/h14" "$staging_root/tmp/h15"
        mkdir -p "$dest"; cp -a "$staging_root$cdest/." "$dest/"
        rm -rf "$staging_root$cdest"
    fi
    [[ -d "$dest/tree_sitter" && -d "$dest/tree_sitter_json" ]] \
        || { echo "prebuild: tree-sitter core/grammars missing after install" >&2; exit 9; }
    echo "prebuild: staged tree-sitter core + 15 grammars into $dest"
}

populate_overlay() {
    local pyver
    pyver="$(ls -d "$staging_root"/usr/lib/python3.* 2>/dev/null | grep -oE 'python3\.[0-9]+' | head -1)"

    # interpreter + its .so closure
    copy_to_overlay /usr/bin/python3 0755
    copy_so_closure /usr/bin/python3
    ln -sf python3 "$overlay_dir/usr/bin/python" 2>/dev/null || true

    # stdlib (+ each lib-dynload C-extension carries its own .so deps)
    mkdir -p "$overlay_dir/usr/lib/$pyver"
    cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    if [[ -d "$staging_root/usr/lib/$pyver/lib-dynload" ]]; then
        for so in "$staging_root/usr/lib/$pyver/lib-dynload"/*.so; do
            [[ -e "$so" ]] && copy_so_closure "/usr/lib/$pyver/lib-dynload/$(basename "$so")"
        done
    fi

    # musl loader search path
    if [[ -f "$staging_root/etc/ld-musl-${arch}.path" ]]; then
        install -Dm0644 "$staging_root/etc/ld-musl-${arch}.path" "$overlay_dir/etc/ld-musl-${arch}.path"
    else
        mkdir -p "$overlay_dir/etc"; printf '/lib\n/usr/lib\n' > "$overlay_dir/etc/ld-musl-${arch}.path"
    fi

    # leg A: pinned pure-Python textual+casca closure -> /opt/pytui (PYTHONPATH-first at runtime)
    provision_site_packages "$overlay_dir/opt/pytui" "$ASSETS/requirements.txt" \
        textual casca rich markdown_it mdurl pygments typing_extensions platformdirs
    # leg B: two REAL Textualize apps, each in its OWN target dir (their textual pins conflict:
    # 0.58.1 for toolong, 0.43.2 for frogmouth, 8.2.8 for leg A).
    provision_site_packages "$overlay_dir/opt/pytui-toolong" "$ASSETS/requirements-toolong.txt" \
        toolong textual rich click markdown_it
    provision_site_packages "$overlay_dir/opt/pytui-frogmouth" "$ASSETS/requirements-frogmouth.txt" \
        frogmouth textual httpx xdg

    # leg B (heavy, game): textual-tetris — a REAL interactive textual game; whole closure is pure
    # Python (py3-none-any), arch-independent, ZERO C-extensions. Its OWN target dir (textual 8.2.8).
    provision_site_packages "$overlay_dir/opt/pytui-game" "$ASSETS/requirements-game.txt" \
        textris textual rich markdown_it

    # leg B (heavy, app): posting — the heaviest real textual API user. Split provisioning into ONE
    # target dir /opt/pytui-posting: (1) pinned PURE-Python closure via host pip (arch-independent);
    # (2) its C-extension deps (pydantic-core/watchfiles/brotli/pyyaml) musl-native from Alpine apk,
    # prebuilt for all four arches; (3) tree-sitter core + grammars for textual[syntax] TextArea
    # highlighting — musllinux wheels where present, else built from source under qemu-user-static.
    provision_site_packages "$overlay_dir/opt/pytui-posting" "$ASSETS/requirements-posting.txt" \
        posting textual textual_autocomplete pydantic_settings httpx rich click
    provision_posting_cext "$overlay_dir/opt/pytui-posting"
    provision_posting_treesitter "$overlay_dir/opt/pytui-posting"

    # carpet sources -> /root/pytui, launcher -> /usr/bin
    mkdir -p "$overlay_dir/root/pytui"
    local n=0
    for f in "$app_dir"/python/*.py; do
        [[ -f "$f" ]] || continue
        install -Dm0644 "$f" "$overlay_dir/root/pytui/$(basename "$f")"
        n=$((n + 1))
    done
    # leg B fixtures live next to the carpets at /root/pytui
    local fx
    for fx in fixture.log fixture.md; do
        [[ -f "$ASSETS/$fx" ]] || { echo "prebuild: missing assets/$fx" >&2; exit 4; }
        install -Dm0644 "$ASSETS/$fx" "$overlay_dir/root/pytui/$fx"
    done
    # posting fixture collection: a committed .posting.yaml request tree next to the carpet (the
    # carpet copies it into a throwaway tempdir at runtime so the app may save freely).
    [[ -d "$ASSETS/posting-collection" ]] || { echo "prebuild: missing assets/posting-collection" >&2; exit 4; }
    mkdir -p "$overlay_dir/root/pytui/posting-collection"
    cp -a "$ASSETS/posting-collection/." "$overlay_dir/root/pytui/posting-collection/"
    install -Dm0755 "$PROG/run-pytui.sh" "$overlay_dir/usr/bin/run-pytui.sh"
    echo "prebuild: staged $n python module(s) + fixtures (log/md + posting-collection); $pyver TUI overlay ready for $arch"
}

ensure_host_tools
grow_rootfs
extract_base_rootfs
install_python
populate_overlay
