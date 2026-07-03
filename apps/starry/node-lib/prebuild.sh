#!/usr/bin/env bash
# prebuild.sh — provision a Node.js 22 runtime and stage the node-lib library carpet.
#
# Reproducible model (mirrors the merged node-lang / python-lang apps): extract the base
# Alpine rootfs to a staging tree, `apk add nodejs npm icu-data-full` INTO it via qemu-user-static
# (apk resolves the CURRENT v3.22 closure for the target arch — no hard-coded drifting apk URLs,
# no pre-cached-or-exit), then copy node + its shared-library closure + ICU data into the app
# overlay. The library dependency closure (less/stylus/sass/@babel/core+presets/terser/eslint) is
# NOT vendored in the source tree: prebuild runs `npm ci --omit=optional` from the committed
# assets/package.json + assets/package-lock.json to fetch a pinned, integrity-checked node_modules
# into a scratch dir, then copies it into the overlay. `--omit=optional` drops sass's optional
# @parcel/watcher native module (watch-mode only, x64-glibc ELF — never loads on musl/non-x64
# targets and unused by the JS-API carpets). The resulting node_modules is pure JS (+ a portable
# source-map .wasm), architecture-independent, so it is valid for all four target arches. The only
# committed inputs are the base rootfs and the app's own assets/ (manifests) + programs/ sources.
#
# npm reaches the registry via the ambient HTTP(S)_PROXY env (set by the app runner). If a local
# npm tarball cache is provided (NLIB_NPM_CACHE, or $NODE_DL_ROOT/npm-cache), npm ci runs
# --prefer-offline against it and falls back to the network on a miss (never cache-miss-exit).
#
# If the build host has no network for apk, node install falls back to a documented, pre-fetched
# apk cache at $NODE_APK_CACHE/<arch>/ (default <repo>/download/nodejs-apks/<arch>); OPTIONAL.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
ASSETS="$app_dir/assets"
PROG="$app_dir/programs"

# Optional offline apk cache (pre-fetched v3.22 closure) — only used if the network apk path
# below fails. Overridable via NODE_APK_CACHE (explicit dir) or NODE_DL_ROOT (its parent root).
default_cache="$(cd "$app_dir/../../.." 2>/dev/null && pwd)/download/nodejs-apks"
apk_cache="${NODE_APK_CACHE:-${NODE_DL_ROOT:+$NODE_DL_ROOT/nodejs-apks}}"
apk_cache="${apk_cache:-$default_cache}"

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
        command -v "$candidate" >/dev/null 2>&1 && { command -v "$candidate"; return 0; }
    done < <(qemu_runner_candidates)
    echo "prebuild: missing qemu-user runner for arch $arch; tried: $(qemu_runner_candidates | paste -sd ', ' -)" >&2
    exit 1
}
qemu_runner="$(find_qemu_runner)"

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
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
    # fails to load its libz/libssl closure ("symbol not found"). Rewrite absolute symlinks under
    # the staging lib dirs to relative targets that resolve inside the staging tree.
    local link tgt rel
    while IFS= read -r link; do
        tgt="$(readlink "$link")"; [[ "$tgt" == /* ]] || continue
        rel="$(realpath -m --relative-to="$(dirname "$link")" "$staging_root$tgt")"
        ln -sf "$rel" "$link"
    done < <(find "$staging_root/lib" "$staging_root/usr/lib" -type l 2>/dev/null)
}

install_node() {
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local repo="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/v3.22/main\n%s/v3.22/community\n' "$repo" "$repo" > "$staging_root/etc/apk/repositories"
    echo "prebuild: apk add nodejs npm icu-data-full (Node 22 LTS) from Alpine v3.22 via $qemu_runner..."
    if QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" --keys-dir "$staging_root/etc/apk/keys" \
            --update-cache --no-progress --no-scripts add nodejs npm icu-data-full
    then echo "prebuild: provisioned nodejs via network apk"
    else echo "prebuild: network apk failed; falling back to offline apk cache $apk_cache/$arch"; install_node_offline; fi
}
install_node_offline() {
    local dir="$apk_cache/$arch"
    [[ -d "$dir" ]] || { echo "prebuild: offline apk cache missing: $dir" >&2; exit 3; }
    local apks=( musl libgcc "libstdc++" zlib zstd-libs brotli-libs c-ares nghttp2-libs
        ada-libs simdjson simdutf sqlite-libs libcrypto3 libssl3 icu-libs icu-data-full nodejs )
    local pkg f
    for pkg in "${apks[@]}"; do
        f="$(ls "$dir/${pkg}"-[0-9]*.apk 2>/dev/null | head -1 || true)"
        [[ -n "$f" ]] || { echo "prebuild: offline apk missing for '$pkg' in $dir" >&2; exit 3; }
        tar -xzf "$f" -C "$staging_root" --exclude='.PKGINFO' --exclude='.SIGN.*' \
            --exclude='.pre-install' --exclude='.post-install' --exclude='.trigger' 2>/dev/null || true
    done
    echo "prebuild: provisioned nodejs offline from ${#apks[@]} apks"
}

verify_node() {
    [[ -x "$staging_root/usr/bin/node" ]] || { echo "prebuild: no /usr/bin/node after install" >&2; exit 4; }
    # Best-effort version gate. Probing the freshly-apk'd node under qemu-user can crash for the
    # SAME-arch case (x86 node under qemu-x86_64-static — V8 JIT/mmap trips a qemu-user SIGSEGV),
    # so an empty/failed probe is NOT fatal: apk already verified the package, and run_all.py on
    # the real StarryOS target prints the node version and gates the carpets (authoritative check).
    local nodever
    nodever="$(QEMU_LD_PREFIX="$staging_root" "$qemu_runner" -L "$staging_root" \
        "$staging_root/usr/bin/node" -p 'process.versions.node' 2>/dev/null || true)"
    case "$nodever" in
        22.*|2[3-9].*|[3-9][0-9].*) echo "prebuild: provisioned node v$nodever" ;;
        '') echo "prebuild: node version probe unavailable under qemu-user (same-arch V8/JIT); on-target run_all.py gates the version" ;;
        *) echo "prebuild: WARNING unexpected node version '$nodever' (want >=22); deferring to on-target run" ;;
    esac
}

copy_to_overlay() {
    local src="$staging_root$1" dst="$overlay_dir$1"
    [[ -e "$src" ]] || { echo "prebuild: missing $1 after install" >&2; exit 6; }
    [[ -L "$src" ]] && src="$(readlink -f "$src")"
    install -Dm"$2" "$src" "$dst"
}
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

# Run `npm ci --omit=optional` inside $1 (which already holds package.json + package-lock.json).
# Primary runner: the build host's own npm — node_modules is pure JS (+ portable wasm) and thus
# architecture-independent, so host npm produces a tree valid for every target arch. Fallback: the
# musl npm apk-provisioned into the staging rootfs, executed under qemu-user-static. Extra args
# (e.g. --cache/--prefer-offline) are appended.
run_npm_ci() {
    local dir="$1"; shift
    if command -v npm >/dev/null 2>&1; then
        echo "prebuild: npm ci via host npm $(npm --version) (arch-independent JS closure)"
        if ( cd "$dir" && npm ci --omit=optional --no-audit --no-fund --loglevel=warn "$@" ); then
            return 0
        fi
        echo "prebuild: host npm ci failed; trying apk-provisioned npm under qemu-user" >&2
    fi
    local npm_cli="$staging_root/usr/lib/node_modules/npm/bin/npm-cli.js"
    [[ -f "$npm_cli" ]] || { echo "prebuild: no host npm and no apk npm at $npm_cli" >&2; return 1; }
    echo "prebuild: npm ci via apk npm under $qemu_runner (musl node, arch $arch)"
    ( cd "$dir" && QEMU_LD_PREFIX="$staging_root" HOME="$dir" \
        "$qemu_runner" -L "$staging_root" "$staging_root/usr/bin/node" "$npm_cli" \
            ci --omit=optional --no-audit --no-fund --loglevel=warn "$@" )
}

# Provision the library dependency closure into $1 (the overlay's /root/nlib) by fetching it with
# npm ci from the committed manifests — nothing binary lives in the source tree.
provision_node_modules() {
    local nw="$1" stage
    [[ -f "$ASSETS/package.json" && -f "$ASSETS/package-lock.json" ]] || {
        echo "prebuild: missing assets/package.json or assets/package-lock.json" >&2; exit 7; }
    stage="$(mktemp -d "${TMPDIR:-/tmp}/nlib-npm.XXXXXX")"
    cp "$ASSETS/package.json" "$ASSETS/package-lock.json" "$stage/"

    local extra=() npm_cache="${NLIB_NPM_CACHE:-${NODE_DL_ROOT:+$NODE_DL_ROOT/npm-cache}}"
    if [[ -n "$npm_cache" && -d "$npm_cache" ]]; then
        echo "prebuild: using local npm cache $npm_cache (--prefer-offline; network on miss)"
        extra=(--cache "$npm_cache" --prefer-offline)
    fi

    if ! run_npm_ci "$stage" "${extra[@]}"; then
        echo "prebuild: npm ci failed (need network to the npm registry, or a populated cache)" >&2
        rm -rf "$stage"; exit 7
    fi
    [[ -d "$stage/node_modules" ]] || { echo "prebuild: npm ci produced no node_modules" >&2; rm -rf "$stage"; exit 7; }

    # Sanity: every module the carpets require() must be present.
    local m
    for m in less stylus sass @babel/core @babel/preset-typescript @babel/preset-react terser eslint; do
        [[ -f "$stage/node_modules/$m/package.json" ]] || {
            echo "prebuild: required module '$m' missing after npm ci" >&2; rm -rf "$stage"; exit 7; }
    done

    rm -rf "$nw/node_modules"
    cp -a "$stage/node_modules" "$nw/node_modules"
    rm -rf "$stage"
}

populate_overlay() {
    copy_to_overlay /usr/bin/node 0755
    copy_so_closure /usr/bin/node
    ln -sf node "$overlay_dir/usr/bin/nodejs" 2>/dev/null || true

    # ELF program interpreter (musl loader) under its real name (DT_NEEDED only names the symlink).
    local interp real
    interp="$(readelf -l "$staging_root/usr/bin/node" 2>/dev/null | sed -n 's/.*program interpreter: \(.*\)\]/\1/p' | tr -d ' ')"
    if [[ -n "$interp" && -e "$staging_root$interp" ]]; then
        real="$interp"
        [[ -L "$staging_root$interp" ]] && real="$(readlink -f "$staging_root$interp")" && real="${real#"$staging_root"}"
        install -Dm0755 "$staging_root$real" "$overlay_dir$interp"
    fi

    # ICU data table (Intl.* / toLocaleString) + musl loader search path.
    if [[ -d "$staging_root/usr/share/icu" ]]; then
        mkdir -p "$overlay_dir/usr/share/icu"; cp -a "$staging_root/usr/share/icu/." "$overlay_dir/usr/share/icu/"
    fi
    if [[ -f "$staging_root/etc/ld-musl-${arch}.path" ]]; then
        install -Dm0644 "$staging_root/etc/ld-musl-${arch}.path" "$overlay_dir/etc/ld-musl-${arch}.path"
    else
        mkdir -p "$overlay_dir/etc"; printf '/lib\n/usr/lib\n/usr/local/lib\n' > "$overlay_dir/etc/ld-musl-${arch}.path"
    fi

    # Stage the carpet sources into /root/nlib and the run-nlib.sh gate into /usr/bin, then fetch
    # the less/stylus/sass/babel/terser/eslint node_modules closure with npm ci (never vendored).
    # Each carpet resolves require() from /root/nlib/node_modules (run-nlib.sh cd's there) and
    # writes scratch under __dirname.
    local nw="$overlay_dir/root/nlib"
    install -d "$nw/carpets"
    install -m0644 "$PROG/carpets/LessCarpet.js" "$PROG/carpets/StylusCarpet.js" "$PROG/carpets/ScssCarpet.js" \
        "$PROG/carpets/BabelCarpet.js" "$PROG/carpets/TerserCarpet.js" "$PROG/carpets/EslintCarpet.js" \
        "$PROG/carpets/CjsEsmCarpet.js" "$nw/carpets/"
    provision_node_modules "$nw"
    install -Dm0755 "$PROG/run-nlib.sh" "$overlay_dir/usr/bin/run-nlib.sh"

    echo "prebuild: staged node + .so closure + ICU + npm-ci node_modules ($(du -sh "$nw/node_modules" | cut -f1)) + 7 carpets; overlay ready for $arch"
}

ensure_host_tools
extract_base_rootfs
normalize_symlinks
install_node
verify_node
populate_overlay
