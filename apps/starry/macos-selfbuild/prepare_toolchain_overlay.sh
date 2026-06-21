#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

output_dir="${STARRY_TOOLCHAIN_OVERLAY_DIR:-$repo_root/target/starry-macos-selfbuild/rootfs-build/toolchain-overlay}"
source_dir="$repo_root"
rust_toolchain="${RUST_TOOLCHAIN:-nightly-2026-05-28}"
alpine_branch="${ALPINE_BRANCH:-v3.23}"
alpine_arch="${ALPINE_ARCH:-aarch64}"
alpine_mirror="${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
rust_dist_server="${RUST_DIST_SERVER:-https://static.rust-lang.org}"
cargo_registry_index="${STARRY_CARGO_REGISTRY_INDEX:-${CARGO_REGISTRY_INDEX:-}}"
guest_target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
force=0

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/prepare_toolchain_overlay.sh [--output DIR] [--force]

Internal/helper stage: downloads and prepares the AArch64 guest toolchain
overlay used by the macOS StarryOS self-build app.

The output is a filesystem tree, not a rootfs image. build_rootfs.sh refreshes
this cache, and run_selfbuild.sh injects a per-run overlay copy into the copied
work rootfs before booting QEMU.

Environment:
  ALPINE_BRANCH        Alpine branch for APK packages (default: v3.23)
  ALPINE_MIRROR        Alpine mirror URL
  RUST_DIST_SERVER     Rust dist server URL (default: https://static.rust-lang.org)
  STARRY_CARGO_REGISTRY_INDEX
                       Optional Cargo registry index URL, e.g. sparse+https://rsproxy.cn/index/
  RUST_TOOLCHAIN       Rust toolchain date/name (default: nightly-2026-05-28)
  BUILD_TARGET         Guest Cargo target to prefetch (default: aarch64-unknown-none-softfloat)
USAGE
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --output)
            output_dir="$2"
            shift 2
            ;;
        --source)
            source_dir="$2"
            shift 2
            ;;
        --force)
            force=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

for cmd in awk cargo curl find sed tar; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "$cmd not found" >&2
        exit 1
    }
done

if [[ ! -f "$source_dir/Cargo.toml" ]]; then
    echo "source dir does not look like TGOSKits: $source_dir" >&2
    exit 1
fi
source_dir="$(cd "$source_dir" && pwd)"

work_dir="$repo_root/target/starry-macos-selfbuild/rootfs-build"
cache_alpine_branch="${alpine_branch//\//_}"
cache_rust_toolchain="${rust_toolchain//\//_}"
apk_cache_dir="$work_dir/apk-cache-${cache_alpine_branch}-${alpine_arch}"
rust_dist_dir="$work_dir/rust-dist-${cache_rust_toolchain}"
cargo_home_dir="$work_dir/cargo-home"
prefetch_source_dir="$work_dir/prefetch-source"
extra_fetch_dir="$work_dir/extra-fetch"
extra_fetch_workspace_dir="$work_dir/extra-fetch-workspace"
marker_file="$output_dir/.starry-macos-toolchain-overlay"

sha256_file() {
    local path="$1"
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{ print $1 }'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{ print $1 }'
    else
        cksum "$path" | awk '{ print $1 }'
    fi
}

overlay_signature() {
    local cargo_lock_sha
    cargo_lock_sha="$(sha256_file "$source_dir/Cargo.lock")"
    cat <<EOF
rust_toolchain=$rust_toolchain
alpine_branch=$alpine_branch
alpine_arch=$alpine_arch
guest_target=$guest_target
cargo_lock_sha256=$cargo_lock_sha
cargo_registry_index=$cargo_registry_index
EOF
}

overlay_has_required_files() {
    [[ -x "$output_dir/opt/cargo-nightly-sysroot" ]] \
        && [[ -x "$output_dir/opt/rustc-nightly-sysroot" ]] \
        && [[ -x "$output_dir/opt/rustdoc-nightly-sysroot" ]] \
        && [[ -x "$output_dir/usr/bin/aarch64-linux-musl-gcc" ]] \
        && [[ -e "$output_dir/usr/bin/aarch64-linux-musl-cc" ]] \
        && [[ -x "$output_dir/usr/bin/aarch64-linux-musl-ar" ]] \
        && [[ -f "$output_dir/opt/rust-nightly/lib/rustlib/src/rust/library/Cargo.lock" ]] \
        && [[ -d "$output_dir/root/.cargo/registry" ]]
}

overlay_is_fresh() {
    [[ "$force" = "0" ]] || return 1
    [[ -f "$marker_file" ]] || return 1
    overlay_has_required_files || return 1
    diff -q "$marker_file" <(overlay_signature) >/dev/null 2>&1
}

download_file() {
    local url="$1"
    local out="$2"
    local tmp="${out}.part"

    rm -f "$tmp"
    if curl -fL \
        --connect-timeout 20 \
        --speed-limit 1024 \
        --speed-time 60 \
        --retry 5 \
        --retry-delay 3 \
        --retry-all-errors \
        -o "$tmp" \
        "$url"; then
        mv "$tmp" "$out"
        return 0
    fi
    rm -f "$tmp"
    return 1
}

download_apk_index() {
    local repo="$1"
    local out="$apk_cache_dir/APKINDEX.$repo"
    local url="$alpine_mirror/$alpine_branch/$repo/$alpine_arch/APKINDEX.tar.gz"

    mkdir -p "$apk_cache_dir"
    if [[ ! -f "$out" ]]; then
        echo "downloading Alpine APKINDEX: $repo"
        download_file "$url" "$apk_cache_dir/APKINDEX.$repo.tar.gz"
        tar -xzf "$apk_cache_dir/APKINDEX.$repo.tar.gz" -C "$apk_cache_dir" APKINDEX
        mv "$apk_cache_dir/APKINDEX" "$out"
    fi
}

apk_field() {
    local package="$1"
    local field="$2"
    awk -v pkg="$package" -v field="$field" '
        BEGIN { RS=""; FS="\n" }
        {
            name = ""
            value = ""
            for (i = 1; i <= NF; i++) {
                if ($i ~ /^P:/) name = substr($i, 3)
                if ($i ~ ("^" field ":")) value = substr($i, 3)
            }
            if (name == pkg) {
                print value
                exit
            }
        }
    ' "$apk_cache_dir/APKINDEX.main" "$apk_cache_dir/APKINDEX.community"
}

apk_provider() {
    local dep="$1"
    awk -v dep="$dep" '
        BEGIN { RS=""; FS="\n" }
        {
            name = ""
            provides = ""
            for (i = 1; i <= NF; i++) {
                if ($i ~ /^P:/) name = substr($i, 3)
                if ($i ~ /^p:/) provides = substr($i, 3)
            }
            split(provides, p, " ")
            for (i in p) {
                item = p[i]
                sub(/[<>=~].*/, "", item)
                if (item == dep) {
                    print name
                    exit
                }
            }
        }
    ' "$apk_cache_dir/APKINDEX.main" "$apk_cache_dir/APKINDEX.community"
}

apk_repo_for() {
    local package="$1"
    local repo
    for repo in main community; do
        if awk -v pkg="$package" '
            BEGIN { RS=""; FS="\n"; found = 0 }
            { for (i = 1; i <= NF; i++) if ($i == "P:" pkg) found = 1 }
            END { exit found ? 0 : 1 }
        ' "$apk_cache_dir/APKINDEX.$repo"; then
            printf '%s\n' "$repo"
            return
        fi
    done
}

normalize_dep() {
    local dep="$1"
    dep="${dep#!}"
    dep="${dep%%[<>=~]*}"
    printf '%s\n' "$dep"
}

rust_dist_date() {
    printf '%s\n' "${rust_toolchain#nightly-}"
}

rust_dist_base() {
    printf '%s/dist/%s\n' "${rust_dist_server%/}" "$(rust_dist_date)"
}

install_rust_dist_component() {
    local component="$1"
    local toolchain="$2"
    local target="$3"
    local channel="${toolchain%%-*}"
    local archive="$component-$channel-$target.tar.xz"
    local archive_dir="${archive%.tar.xz}"
    local url

    url="$(rust_dist_base)/$archive"
    mkdir -p "$rust_dist_dir"
    if [[ ! -f "$rust_dist_dir/$archive" ]]; then
        echo "downloading Rust component: $archive"
        rm -f "$rust_dist_dir/$archive.part"
        download_file "$url" "$rust_dist_dir/$archive.part"
        mv "$rust_dist_dir/$archive.part" "$rust_dist_dir/$archive"
    fi

    rm -rf "$rust_dist_dir/$archive_dir"
    tar -xJf "$rust_dist_dir/$archive" -C "$rust_dist_dir"
    (cd "$rust_dist_dir/$archive_dir" && ./install.sh --prefix="$output_dir/opt/rust-nightly" --disable-ldconfig)
}

install_rust_src_component() {
    local toolchain="$1"
    local channel="${toolchain%%-*}"
    local archive="rust-src-$channel.tar.xz"
    local archive_dir="${archive%.tar.xz}"
    local url

    url="$(rust_dist_base)/$archive"
    mkdir -p "$rust_dist_dir"
    if [[ ! -f "$rust_dist_dir/$archive" ]]; then
        echo "downloading Rust component: $archive"
        rm -f "$rust_dist_dir/$archive.part"
        download_file "$url" "$rust_dist_dir/$archive.part"
        mv "$rust_dist_dir/$archive.part" "$rust_dist_dir/$archive"
    fi

    rm -rf "$rust_dist_dir/$archive_dir"
    tar -xJf "$rust_dist_dir/$archive" -C "$rust_dist_dir"
    (cd "$rust_dist_dir/$archive_dir" && ./install.sh --prefix="$output_dir/opt/rust-nightly" --disable-ldconfig)
}

cargo_fetch() {
    env -u CARGO_REGISTRY_INDEX "$@"
}

finish_toolchain_overlay() {
    local libclang_path cargo_env path prefetch_manifest

    mkdir -p "$cargo_home_dir" "$output_dir/root/.cargo" "$output_dir/opt" "$output_dir/usr/bin"

    libclang_path="$(find "$output_dir/usr/lib" -name "libclang.so*" | head -1 || true)"
    if [[ -n "$libclang_path" && ! -e "$output_dir/usr/lib/libclang.so" ]]; then
        ln -s "${libclang_path#$output_dir/usr/lib/}" "$output_dir/usr/lib/libclang.so"
    fi

    cat >"$output_dir/opt/rustc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustc --sysroot /opt/rust-nightly "$@"
WRAP
    cat >"$output_dir/opt/rustdoc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustdoc --sysroot /opt/rust-nightly "$@"
WRAP
    cat >"$output_dir/opt/cargo-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/cargo "$@"
WRAP
    chmod +x "$output_dir/opt/rustc-nightly-sysroot" "$output_dir/opt/rustdoc-nightly-sysroot" "$output_dir/opt/cargo-nightly-sysroot"

    cat >"$output_dir/usr/bin/aarch64-linux-musl-gcc" <<'WRAP'
#!/bin/sh
if [ "${1:-}" = "-print-sysroot" ]; then
    printf '%s\n' /usr
    exit 0
fi

args=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target=aarch64-unknown-none|--target=aarch64-unknown-none-softfloat)
            shift
            ;;
        --target)
            if [ "${2:-}" = "aarch64-unknown-none" ] || [ "${2:-}" = "aarch64-unknown-none-softfloat" ]; then
                shift 2
            else
                args="$args $1"
                shift
            fi
            ;;
        *)
            args="$args $1"
            shift
            ;;
    esac
done

exec /usr/bin/gcc $args
WRAP
    chmod +x "$output_dir/usr/bin/aarch64-linux-musl-gcc"
    ln -sf aarch64-linux-musl-gcc "$output_dir/usr/bin/aarch64-linux-musl-cc"
    cat >"$output_dir/usr/bin/aarch64-linux-musl-ar" <<'WRAP'
#!/bin/sh
exec /usr/bin/ar "$@"
WRAP
    chmod +x "$output_dir/usr/bin/aarch64-linux-musl-ar"
    if [[ ! -e "$output_dir/usr/bin/cargo" ]]; then
        ln -s /opt/cargo-nightly-sysroot "$output_dir/usr/bin/cargo"
    fi

    cat >"$cargo_home_dir/config.toml" <<'CARGO_CFG'
[net]
git-fetch-with-cli = true
CARGO_CFG
    if [[ -n "$cargo_registry_index" ]]; then
        cat >>"$cargo_home_dir/config.toml" <<CARGO_CFG

[source.crates-io]
replace-with = "starry-mirror"

[source.starry-mirror]
registry = "$cargo_registry_index"
CARGO_CFG
    fi

    cargo_env=(
        "CARGO_HOME=$cargo_home_dir"
        "CARGO_HTTP_MULTIPLEXING=false"
        "CARGO_HTTP_TIMEOUT=120"
        "CARGO_NET_RETRY=5"
        "RUSTC_BOOTSTRAP=1"
    )
    if [[ -z "$cargo_registry_index" ]]; then
        cargo_env+=("CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse")
    fi

    rm -rf "$prefetch_source_dir"
    mkdir -p "$prefetch_source_dir"
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .cargo apps bootloader components drivers memory net os platforms scripts test-suit tools vendor virtualization xtask; do
        if [[ -e "$source_dir/$path" ]]; then
            cp -a "$source_dir/$path" "$prefetch_source_dir/"
        fi
    done
    if [[ -f "$prefetch_source_dir/.cargo/config.toml" ]]; then
        sed -i.bak "s#$source_dir/vendor#$prefetch_source_dir/vendor#g" "$prefetch_source_dir/.cargo/config.toml" || true
        rm -f "$prefetch_source_dir/.cargo/config.toml.bak"
    fi
    prefetch_manifest="$prefetch_source_dir/Cargo.toml"

    cargo_fetch "${cargo_env[@]}" \
        cargo fetch --target "$guest_target" --manifest-path "$prefetch_manifest"

    rm -rf "$extra_fetch_workspace_dir"
    mkdir -p "$extra_fetch_workspace_dir/src"
    cat >"$extra_fetch_workspace_dir/Cargo.toml" <<'EXTRA_CARGO'
[package]
name = "starry-selfbuild-extra-fetch-workspace"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
EXTRA_CARGO
    awk '
        function emit() {
            if (name != "" && version != "" && source ~ /^registry/) {
                alias = name
                gsub(/[^A-Za-z0-9_]/, "_", alias)
                sub(/\+.*/, "", version)
                printf "extra_%04d_%s = { package = \"%s\", version = \"=%s\" }\n", count, alias, name, version
                count++
            }
        }
        /^\[\[package\]\]/ { emit(); name = ""; version = ""; source = ""; next }
        /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); next }
        /^version = / { version = $0; sub(/^version = "/, "", version); sub(/"$/, "", version); next }
        /^source = / { source = $0; sub(/^source = "/, "", source); sub(/"$/, "", source); next }
        END { emit() }
    ' "$source_dir/Cargo.lock" >>"$extra_fetch_workspace_dir/Cargo.toml"
    cat >>"$extra_fetch_workspace_dir/Cargo.toml" <<'EXTRA_CARGO'

[workspace]
EXTRA_CARGO
    : >"$extra_fetch_workspace_dir/src/lib.rs"
    cargo_fetch "${cargo_env[@]}" \
        cargo fetch --manifest-path "$extra_fetch_workspace_dir/Cargo.toml"

    if [[ -f "$output_dir/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml" ]]; then
        cargo_fetch "${cargo_env[@]}" \
            cargo fetch --locked --manifest-path "$output_dir/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml"
    fi

    rm -rf "$extra_fetch_dir"
    mkdir -p "$extra_fetch_dir/src"
    cat >"$extra_fetch_dir/Cargo.toml" <<'EXTRA_CARGO'
[package]
name = "starry-selfbuild-extra-fetch"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
EXTRA_CARGO
    awk '
        function emit() {
            if (name != "" && version != "" && source ~ /^registry/) {
                alias = name
                gsub(/[^A-Za-z0-9_]/, "_", alias)
                sub(/\+.*/, "", version)
                printf "extra_%04d_%s = { package = \"%s\", version = \"=%s\" }\n", count, alias, name, version
                count++
            }
        }
        /^\[\[package\]\]/ { emit(); name = ""; version = ""; source = ""; next }
        /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name); next }
        /^version = / { version = $0; sub(/^version = "/, "", version); sub(/"$/, "", version); next }
        /^source = / { source = $0; sub(/^source = "/, "", source); sub(/"$/, "", source); next }
        END { emit() }
    ' "$output_dir/opt/rust-nightly/lib/rustlib/src/rust/library/Cargo.lock" >>"$extra_fetch_dir/Cargo.toml"
    cat >>"$extra_fetch_dir/Cargo.toml" <<'EXTRA_CARGO'

[workspace]
EXTRA_CARGO
    : >"$extra_fetch_dir/src/lib.rs"
    cargo_fetch "${cargo_env[@]}" \
        cargo fetch --manifest-path "$extra_fetch_dir/Cargo.toml"

    rm -rf "$output_dir/root/.cargo"
    mkdir -p "$output_dir/root/.cargo"
    cp -a "$cargo_home_dir"/. "$output_dir/root/.cargo"/

    cat >"$output_dir/root/.cargo/config.toml" <<'CARGO_CFG'
[net]
git-fetch-with-cli = true
offline = true
CARGO_CFG
    if [[ -n "$cargo_registry_index" ]]; then
        cat >>"$output_dir/root/.cargo/config.toml" <<CARGO_CFG

[source.crates-io]
replace-with = "starry-mirror"

[source.starry-mirror]
registry = "$cargo_registry_index"
CARGO_CFG
    fi

    cat >"$output_dir/opt/toolchain-info.txt" <<TOOLCHAIN
rust_toolchain=$rust_toolchain
rust_dist_server=$rust_dist_server
alpine_branch=$alpine_branch
alpine_mirror=$alpine_mirror
TOOLCHAIN
}

build_toolchain_overlay() {
    local package queue seen_file pkg dep dep_pkg version repo apk_name apk_url apk_file

    echo "building AArch64 guest toolchain overlay natively on macOS..."
    rm -rf "$output_dir"
    mkdir -p "$output_dir" "$apk_cache_dir" "$rust_dist_dir"

    download_apk_index main
    download_apk_index community

    queue=(
        alpine-baselayout
        alpine-baselayout-data
        alpine-keys
        apk-tools
        busybox
        musl
        libgcc
        libstdc++
        zlib
        zstd-libs
        openssl
        ca-certificates
        ca-certificates-bundle
        bash
        coreutils
        curl
        findutils
        grep
        sed
        gawk
        tar
        xz
        git
        make
        cmake
        pkgconf
        binutils
        build-base
        clang
        clang-libclang
        lld
        llvm
        rust
        cargo
        rust-src
    )

    seen_file="$work_dir/apk-seen.txt"
    : >"$seen_file"

    while ((${#queue[@]} > 0)); do
        pkg="${queue[0]}"
        queue=("${queue[@]:1}")
        [[ -z "$pkg" ]] && continue
        if grep -qx "$pkg" "$seen_file"; then
            continue
        fi

        version="$(apk_field "$pkg" V || true)"
        if [[ -z "$version" ]]; then
            dep_pkg="$(apk_provider "$pkg" || true)"
            if [[ -n "$dep_pkg" ]]; then
                queue+=("$dep_pkg")
                continue
            fi
            echo "warning: Alpine package/provider not found: $pkg" >&2
            continue
        fi

        printf '%s\n' "$pkg" >>"$seen_file"
        repo="$(apk_repo_for "$pkg")"
        apk_name="$pkg-$version.apk"
        apk_url="$alpine_mirror/$alpine_branch/$repo/$alpine_arch/$apk_name"
        apk_file="$apk_cache_dir/$apk_name"

        if [[ ! -f "$apk_file" ]]; then
            echo "downloading APK: $pkg"
            download_file "$apk_url" "$apk_file"
        fi

        tar -xzf "$apk_file" -C "$output_dir" --exclude='.SIGN.*' --exclude='.PKGINFO' --exclude='.INSTALL' || {
            echo "failed to extract APK: $apk_file" >&2
            rm -f "$apk_file"
            exit 1
        }

        for dep in $(apk_field "$pkg" D || true); do
            dep="$(normalize_dep "$dep")"
            [[ -z "$dep" ]] && continue
            queue+=("$dep")
        done
    done

    install_rust_dist_component rustc "$rust_toolchain" aarch64-unknown-linux-musl
    install_rust_dist_component cargo "$rust_toolchain" aarch64-unknown-linux-musl
    install_rust_dist_component rust-std "$rust_toolchain" aarch64-unknown-linux-musl
    install_rust_src_component "$rust_toolchain"

    finish_toolchain_overlay
}

if overlay_is_fresh; then
    echo "toolchain overlay already fresh at $output_dir"
    exit 0
fi

mkdir -p "$work_dir"

rm -rf "$output_dir"
mkdir -p "$output_dir"
build_toolchain_overlay

overlay_signature >"$marker_file"
if ! overlay_has_required_files; then
    echo "prepared toolchain overlay is incomplete: $output_dir" >&2
    exit 1
fi

echo "toolchain_overlay=$output_dir"
