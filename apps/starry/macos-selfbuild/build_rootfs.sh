#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/build_rootfs.sh \
    [--base-rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img] \
    [--toolchain-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img] \
    [--selfbuild-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img] \
    [--source .] \
    [--payload /path/to/starry-macos-selfbuild-payload.tar.zst]

  ROOTFS_PAYLOAD_URL=https://.../starry-macos-selfbuild-payload.tar.zst \
    apps/starry/macos-selfbuild/build_rootfs.sh

Maintainer-only payload rebuild:

  apps/starry/macos-selfbuild/build_rootfs.sh --build-payload-with-docker

Builds the rootfs set used by the macOS HVF self-build app:

  1. ensure/copy the managed AArch64 Alpine rootfs;
  2. resize the copy so it can hold Rust/Cargo and offline Cargo cache;
  3. build or extract an AArch64 guest toolchain payload on macOS;
  4. inject that payload with debugfs to create the toolchain rootfs;
  5. inject the current TGOSKits source tree to create the self-build rootfs.

The default path is macOS-native and does not use Docker. It downloads Alpine
aarch64 APKs and official Rust aarch64-musl toolchain tarballs, prepares the
guest payload under target/, and injects it into the rootfs.

Environment:
  ROOTFS_PAYLOAD       Local payload tarball to inject
  ROOTFS_PAYLOAD_URL   Payload URL to download and inject
  ALPINE_BRANCH        Alpine branch for APK payloads (default: v3.23)
  ALPINE_MIRROR        Alpine mirror URL
  RUST_DIST_SERVER     Rust dist server URL (default: https://static.rust-lang.org)
  STARRY_CARGO_REGISTRY_INDEX
                   Optional Cargo registry index URL, e.g. sparse+https://rsproxy.cn/index/
  RUST_TOOLCHAIN       Rust toolchain date/name (default: nightly-2026-05-28)
  BUILD_TARGET         Guest Cargo target to prefetch (default: aarch64-unknown-none-softfloat)
  DOCKER_IMAGE     Maintainer-only Alpine image for --build-payload-with-docker
                   (default: alpine:v3.23)
  ROOTFS_SIZE_MB   Size of the toolchain image after resize (default: 16384)
  DEBUGFS          Path to debugfs
  E2FSCK           Path to e2fsck
  RESIZE2FS        Path to resize2fs
USAGE
}

base_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
toolchain_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img"
selfbuild_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img"
source_dir="$repo_root"
rust_nightly_dir="${RUST_NIGHTLY_DIR:-}"
rust_toolchain="${RUST_TOOLCHAIN:-nightly-2026-05-28}"
rootfs_payload="${ROOTFS_PAYLOAD:-}"
rootfs_payload_url="${ROOTFS_PAYLOAD_URL:-}"
alpine_branch="${ALPINE_BRANCH:-v3.23}"
alpine_arch="${ALPINE_ARCH:-aarch64}"
alpine_mirror="${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
rust_dist_server="${RUST_DIST_SERVER:-https://static.rust-lang.org}"
cargo_registry_index="${STARRY_CARGO_REGISTRY_INDEX:-${CARGO_REGISTRY_INDEX:-}}"
guest_target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
docker_image="${DOCKER_IMAGE:-alpine:v3.23}"
rootfs_size_mb="${ROOTFS_SIZE_MB:-16384}"
build_payload_with_docker=0

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --base-rootfs)
            base_rootfs="$2"
            shift 2
            ;;
        --toolchain-rootfs)
            toolchain_rootfs="$2"
            shift 2
            ;;
        --selfbuild-rootfs)
            selfbuild_rootfs="$2"
            shift 2
            ;;
        --source)
            source_dir="$2"
            shift 2
            ;;
        --rust-nightly-dir)
            rust_nightly_dir="$2"
            shift 2
            ;;
        --payload)
            rootfs_payload="$2"
            shift 2
            ;;
        --payload-url)
            rootfs_payload_url="$2"
            shift 2
            ;;
        --build-payload-with-docker)
            build_payload_with_docker=1
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

find_tool() {
    local env_name="$1"
    local tool_name="$2"
    local homebrew_path="$3"
    local configured="${!env_name:-}"

    if [[ -n "$configured" ]]; then
        printf '%s\n' "$configured"
    elif command -v "$tool_name" >/dev/null 2>&1; then
        command -v "$tool_name"
    elif [[ -x "$homebrew_path" ]]; then
        printf '%s\n' "$homebrew_path"
    else
        echo "$tool_name not found; install e2fsprogs or set $env_name=/path/to/$tool_name" >&2
        exit 1
    fi
}

copy_image() {
    local src="$1"
    local dst="$2"

    rm -f "$dst"
    if cp -c "$src" "$dst" 2>/dev/null; then
        return
    fi
    if cp --reflink=auto "$src" "$dst" 2>/dev/null; then
        return
    fi
    cp "$src" "$dst"
}

debugfs="$(find_tool DEBUGFS debugfs /opt/homebrew/opt/e2fsprogs/sbin/debugfs)"
e2fsck="$(find_tool E2FSCK e2fsck /opt/homebrew/opt/e2fsprogs/sbin/e2fsck)"
resize2fs="$(find_tool RESIZE2FS resize2fs /opt/homebrew/opt/e2fsprogs/sbin/resize2fs)"
e2fsprogs_bin="$(dirname "$debugfs")"
export PATH="$e2fsprogs_bin:$PATH"

for cmd in awk cargo curl tar find; do
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

if [[ ! -f "$base_rootfs" ]]; then
    echo "base rootfs not found: $base_rootfs"
    echo "running: cargo xtask starry rootfs --arch aarch64"
    (cd "$repo_root" && cargo xtask starry rootfs --arch aarch64)
fi

if [[ ! -f "$base_rootfs" ]]; then
    echo "base rootfs still missing after xtask: $base_rootfs" >&2
    exit 1
fi

if [[ -n "$rust_nightly_dir" && ! -d "$rust_nightly_dir" ]]; then
    echo "rust nightly sysroot dir not found: $rust_nightly_dir" >&2
    exit 1
fi

work_dir="$repo_root/target/starry-macos-selfbuild/rootfs-build"
overlay_dir="$work_dir/overlay"
debugfs_cmd="$work_dir/debugfs-build-rootfs.cmd"
debugfs_log="$work_dir/debugfs-build-rootfs.log"
mkdir -p "$work_dir" "$(dirname "$toolchain_rootfs")" "$(dirname "$selfbuild_rootfs")"
rm -rf "$overlay_dir"
mkdir -p "$overlay_dir"

echo "base_rootfs=$base_rootfs"
echo "toolchain_rootfs=$toolchain_rootfs"
echo "selfbuild_rootfs=$selfbuild_rootfs"
echo "source_dir=$source_dir"
echo "rust_toolchain=$rust_toolchain"
echo "guest_target=$guest_target"
if [[ "$build_payload_with_docker" = "1" ]]; then
    echo "payload_mode=docker"
    echo "docker_image=$docker_image"
elif [[ -n "$rootfs_payload" || -n "$rootfs_payload_url" ]]; then
    echo "payload_mode=artifact"
    echo "rootfs_payload=${rootfs_payload:-}"
    echo "rootfs_payload_url=${rootfs_payload_url:-}"
else
    echo "payload_mode=native"
    echo "alpine_branch=$alpine_branch"
    echo "alpine_arch=$alpine_arch"
    echo "alpine_mirror=$alpine_mirror"
    echo "rust_dist_server=$rust_dist_server"
    if [[ -n "$cargo_registry_index" ]]; then
        echo "cargo_registry_index=$cargo_registry_index"
    fi
fi

echo "copying and resizing base rootfs..."
copy_image "$base_rootfs" "$toolchain_rootfs"
truncate -s "${rootfs_size_mb}M" "$toolchain_rootfs"
"$e2fsck" -fy "$toolchain_rootfs" >/dev/null 2>&1 || true
"$resize2fs" "$toolchain_rootfs" >/dev/null

download_payload() {
    local url="$1"
    local out="$2"

    command -v curl >/dev/null 2>&1 || {
        echo "curl not found; install curl or pass --payload /path/to/payload.tar.*" >&2
        exit 1
    }

    curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$out" "$url"
}

extract_payload() {
    local archive="$1"

    if [[ ! -f "$archive" ]]; then
        echo "payload not found: $archive" >&2
        exit 1
    fi

    case "$archive" in
        *.tar.zst|*.tzst)
            tar --zstd -xf "$archive" -C "$overlay_dir"
            ;;
        *.tar.xz|*.txz)
            tar -xJf "$archive" -C "$overlay_dir"
            ;;
        *.tar.gz|*.tgz)
            tar -xzf "$archive" -C "$overlay_dir"
            ;;
        *.tar)
            tar -xf "$archive" -C "$overlay_dir"
            ;;
        *)
            echo "unsupported payload extension: $archive" >&2
            echo "expected .tar, .tar.gz, .tgz, .tar.xz, .txz, .tar.zst, or .tzst" >&2
            exit 1
            ;;
    esac

    if [[ -d "$overlay_dir/payload" ]]; then
        shopt -s dotglob nullglob
        mv "$overlay_dir"/payload/* "$overlay_dir"/
        rmdir "$overlay_dir/payload"
        shopt -u dotglob nullglob
    fi
}

cache_alpine_branch="${alpine_branch//\//_}"
cache_rust_toolchain="${rust_toolchain//\//_}"
apk_cache_dir="$work_dir/apk-cache-${cache_alpine_branch}-${alpine_arch}"
rust_dist_dir="$work_dir/rust-dist-${cache_rust_toolchain}"
cargo_home_dir="$work_dir/cargo-home"
prefetch_source_dir="$work_dir/prefetch-source"
extra_fetch_dir="$work_dir/extra-fetch"
extra_fetch_aws_dir="$work_dir/extra-fetch-aws"
extra_fetch_workspace_dir="$work_dir/extra-fetch-workspace"

download_apk_index() {
    local repo="$1"
    local out="$apk_cache_dir/APKINDEX.$repo"
    local url="$alpine_mirror/$alpine_branch/$repo/$alpine_arch/APKINDEX.tar.gz"

    mkdir -p "$apk_cache_dir"
    if [[ ! -f "$out" ]]; then
        echo "downloading Alpine APKINDEX: $repo"
        curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$apk_cache_dir/APKINDEX.$repo.tar.gz" "$url"
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
    for repo in main community; do
        if awk -v pkg="$package" '
            BEGIN { RS=""; FS="\n"; found = 0 }
            { for (i = 1; i <= NF; i++) if ($i == "P:" pkg) found = 1 }
            END { exit found ? 0 : 1 }
        ' \
            "$apk_cache_dir/APKINDEX.$repo"; then
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

build_native_payload() {
    local package queue seen_file pkg dep dep_pkg version repo apk_name apk_url apk_file

    echo "building AArch64 guest payload natively on macOS..."
    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir" "$apk_cache_dir" "$rust_dist_dir"

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
            curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$apk_file" "$apk_url"
        fi

        tar -xzf "$apk_file" -C "$overlay_dir" --exclude='.SIGN.*' --exclude='.PKGINFO' --exclude='.INSTALL' || {
            echo "failed to extract APK: $apk_file" >&2
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

    finish_payload_overlay
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
        curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$rust_dist_dir/$archive.part" "$url"
        mv "$rust_dist_dir/$archive.part" "$rust_dist_dir/$archive"
    fi

    rm -rf "$rust_dist_dir/$archive_dir"
    tar -xJf "$rust_dist_dir/$archive" -C "$rust_dist_dir"
    (cd "$rust_dist_dir/$archive_dir" && ./install.sh --prefix="$overlay_dir/opt/rust-nightly" --disable-ldconfig)
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
        curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$rust_dist_dir/$archive.part" "$url"
        mv "$rust_dist_dir/$archive.part" "$rust_dist_dir/$archive"
    fi

    rm -rf "$rust_dist_dir/$archive_dir"
    tar -xJf "$rust_dist_dir/$archive" -C "$rust_dist_dir"
    (cd "$rust_dist_dir/$archive_dir" && ./install.sh --prefix="$overlay_dir/opt/rust-nightly" --disable-ldconfig)
}

finish_payload_overlay() {
    local libclang_path cargo_env path prefetch_manifest

    mkdir -p "$cargo_home_dir" "$overlay_dir/root/.cargo" "$overlay_dir/opt" "$overlay_dir/usr/bin"

    libclang_path="$(find "$overlay_dir/usr/lib" -name "libclang.so*" | head -1 || true)"
    if [[ -n "$libclang_path" && ! -e "$overlay_dir/usr/lib/libclang.so" ]]; then
        ln -s "${libclang_path#$overlay_dir/usr/lib/}" "$overlay_dir/usr/lib/libclang.so"
    fi

    cat >"$overlay_dir/opt/rustc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustc --sysroot /opt/rust-nightly "$@"
WRAP
    cat >"$overlay_dir/opt/rustdoc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustdoc --sysroot /opt/rust-nightly "$@"
WRAP
    chmod +x "$overlay_dir/opt/rustc-nightly-sysroot" "$overlay_dir/opt/rustdoc-nightly-sysroot"

    cat >"$overlay_dir/usr/bin/aarch64-linux-musl-gcc" <<'WRAP'
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
    chmod +x "$overlay_dir/usr/bin/aarch64-linux-musl-gcc"

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
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .cargo apps components drivers memory os platforms scripts test-suit tools vendor virtualization xtask; do
        if [[ -e "$source_dir/$path" ]]; then
            cp -a "$source_dir/$path" "$prefetch_source_dir/"
        fi
    done
    if [[ -f "$prefetch_source_dir/.cargo/config.toml" ]]; then
        sed -i.bak "s#$source_dir/vendor#$prefetch_source_dir/vendor#g" "$prefetch_source_dir/.cargo/config.toml" || true
        rm -f "$prefetch_source_dir/.cargo/config.toml.bak"
    fi
    if [[ -d "$prefetch_source_dir/apps/starry/macos-selfbuild/crates/lwprintf-rs" ]] \
        && ! grep -q "apps/starry/macos-selfbuild/crates/lwprintf-rs" "$prefetch_source_dir/Cargo.toml"; then
        cat >>"$prefetch_source_dir/Cargo.toml" <<'PATCH_CARGO'

[patch.crates-io]
lwprintf-rs = { path = "apps/starry/macos-selfbuild/crates/lwprintf-rs" }
PATCH_CARGO
    fi
    prefetch_manifest="$prefetch_source_dir/Cargo.toml"

    env -u CARGO_REGISTRY_INDEX "${cargo_env[@]}" \
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
    env -u CARGO_REGISTRY_INDEX "${cargo_env[@]}" \
        cargo fetch --manifest-path "$extra_fetch_workspace_dir/Cargo.toml"

    if [[ -f "$overlay_dir/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml" ]]; then
        env -u CARGO_REGISTRY_INDEX "${cargo_env[@]}" \
            cargo fetch --locked --manifest-path "$overlay_dir/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml"
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
extra_atomic_waker = { package = "atomic-waker", version = "=1.1.2" }
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
    ' "$overlay_dir/opt/rust-nightly/lib/rustlib/src/rust/library/Cargo.lock" >>"$extra_fetch_dir/Cargo.toml"
    cat >>"$extra_fetch_dir/Cargo.toml" <<'EXTRA_CARGO'

[workspace]
EXTRA_CARGO
    : >"$extra_fetch_dir/src/lib.rs"
    env -u CARGO_REGISTRY_INDEX "${cargo_env[@]}" \
        cargo fetch --manifest-path "$extra_fetch_dir/Cargo.toml"

    rm -rf "$extra_fetch_aws_dir"
    mkdir -p "$extra_fetch_aws_dir/src"
    cat >"$extra_fetch_aws_dir/Cargo.toml" <<'EXTRA_CARGO'
[package]
name = "starry-selfbuild-extra-fetch-aws"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
aws-lc-rs = "=1.17.0"

[workspace]
EXTRA_CARGO
    : >"$extra_fetch_aws_dir/src/lib.rs"
    env -u CARGO_REGISTRY_INDEX "${cargo_env[@]}" \
        cargo fetch --manifest-path "$extra_fetch_aws_dir/Cargo.toml"

    rm -rf "$overlay_dir/root/.cargo"
    mkdir -p "$overlay_dir/root/.cargo"
    cp -a "$cargo_home_dir"/. "$overlay_dir/root/.cargo"/

    cat >"$overlay_dir/root/.cargo/config.toml" <<'CARGO_CFG'
[net]
git-fetch-with-cli = true
offline = true
CARGO_CFG
    if [[ -n "$cargo_registry_index" ]]; then
        cat >>"$overlay_dir/root/.cargo/config.toml" <<CARGO_CFG

[source.crates-io]
replace-with = "starry-mirror"

[source.starry-mirror]
registry = "$cargo_registry_index"
CARGO_CFG
    fi

    cat >"$overlay_dir/opt/toolchain-info.txt" <<TOOLCHAIN
rust_toolchain=$rust_toolchain
rust_dist_server=$rust_dist_server
TOOLCHAIN
}

if [[ "$build_payload_with_docker" = "1" ]]; then
    command -v docker >/dev/null 2>&1 || {
        echo "docker not found; omit --build-payload-with-docker and pass --payload or ROOTFS_PAYLOAD_URL" >&2
        exit 1
    }

    echo "building AArch64 Alpine Rust/Cargo payload in Docker..."
    docker run --rm --platform linux/arm64 \
    -e "RUST_TOOLCHAIN=$rust_toolchain" \
    -v "$overlay_dir:/payload" \
    -v "$source_dir:/source:ro" \
    "$docker_image" /bin/sh -euxc '
        apk_add() {
            apk_try=1
            while ! apk add --no-cache "$@"; do
                if [ "$apk_try" -ge 5 ]; then
                    exit 1
                fi
                sleep "$((apk_try * 5))"
                apk_try="$((apk_try + 1))"
            done
        }

        apk_add \
            bash ca-certificates coreutils curl findutils grep sed gawk tar xz git make cmake pkgconf \
            build-base clang clang-libclang lld llvm rust cargo rust-src
        update-ca-certificates

        mkdir -p /payload/usr /payload/lib /payload/root/.cargo /payload/opt
        libclang_path="$(find /usr/lib -name "libclang.so*" | head -1 || true)"
        if [ -n "$libclang_path" ] && [ ! -e /usr/lib/libclang.so ]; then
            ln -s "$libclang_path" /usr/lib/libclang.so
        fi
        cp -a /usr/bin /payload/usr/
        cp -a /usr/lib /payload/usr/
        if [ -d /usr/libexec ]; then cp -a /usr/libexec /payload/usr/; fi
        if [ -d /usr/src ]; then cp -a /usr/src /payload/usr/; fi
        cp -a /lib/. /payload/lib/

        rustup_home=/tmp/rustup
        cargo_home=/tmp/cargo
        export RUSTUP_HOME="$rustup_home"
        export CARGO_HOME="$cargo_home"
        mkdir -p "$rustup_home" "$cargo_home"
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --default-toolchain "$RUST_TOOLCHAIN"
        "$cargo_home/bin/rustup" component add rust-src --toolchain "$RUST_TOOLCHAIN"
        mkdir -p /payload/opt/rust-nightly
        cp -a "$rustup_home/toolchains/$RUST_TOOLCHAIN-aarch64-unknown-linux-musl"/. /payload/opt/rust-nightly/

        cat >/payload/opt/rustc-nightly-sysroot <<'"'"'WRAP'"'"'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustc --sysroot /opt/rust-nightly "$@"
WRAP
        cat >/payload/opt/rustdoc-nightly-sysroot <<'"'"'WRAP'"'"'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustdoc --sysroot /opt/rust-nightly "$@"
WRAP
        chmod +x /payload/opt/rustc-nightly-sysroot /payload/opt/rustdoc-nightly-sysroot

        cat >/payload/usr/bin/aarch64-linux-musl-gcc <<'"'"'WRAP'"'"'
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
        chmod +x /payload/usr/bin/aarch64-linux-musl-gcc

        cat >/payload/root/.cargo/config.toml <<'"'"'CARGO_CFG'"'"'
[net]
git-fetch-with-cli = true
CARGO_CFG
        fetch_manifest() {
            manifest="$1"
            fetch_try=1
            while ! CARGO_HOME=/payload/root/.cargo \
                CARGO_HTTP_MULTIPLEXING=false \
                CARGO_HTTP_TIMEOUT=120 \
                CARGO_NET_RETRY=5 \
                RUSTC_BOOTSTRAP=1 \
                /payload/opt/rust-nightly/bin/cargo fetch --manifest-path "$manifest"; do
                if [ "$fetch_try" -ge 5 ]; then
                    exit 1
                fi
                sleep "$((fetch_try * 10))"
                fetch_try="$((fetch_try + 1))"
            done
        }

        fetch_manifest /source/Cargo.toml
        if [ -f /payload/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml ]; then
            fetch_manifest /payload/opt/rust-nightly/lib/rustlib/src/rust/library/sysroot/Cargo.toml
        fi
        cat >/payload/root/.cargo/config.toml <<'"'"'CARGO_CFG'"'"'
[net]
git-fetch-with-cli = true
offline = true
CARGO_CFG

        /payload/opt/rust-nightly/bin/rustc --version > /payload/opt/toolchain-info.txt
        /payload/opt/rust-nightly/bin/cargo --version >> /payload/opt/toolchain-info.txt
        /usr/bin/cargo --version >> /payload/opt/toolchain-info.txt
    '
elif [[ -n "$rootfs_payload" || -n "$rootfs_payload_url" ]]; then
    if [[ -z "$rootfs_payload" ]]; then
        if [[ -z "$rootfs_payload_url" ]]; then
            cat >&2 <<'ERR'
No rootfs payload was provided.

Use one of:

  ROOTFS_PAYLOAD_URL=https://.../starry-macos-selfbuild-payload.tar.zst \
    apps/starry/macos-selfbuild/build_rootfs.sh

  apps/starry/macos-selfbuild/build_rootfs.sh \
    --payload /path/to/starry-macos-selfbuild-payload.tar.zst

Maintainers can explicitly rebuild the payload with:

  apps/starry/macos-selfbuild/build_rootfs.sh --build-payload-with-docker
ERR
            exit 2
        fi
        rootfs_payload="$work_dir/$(basename "${rootfs_payload_url%%\?*}")"
        echo "downloading rootfs payload: $rootfs_payload_url"
        download_payload "$rootfs_payload_url" "$rootfs_payload"
    fi

    echo "extracting AArch64 guest toolchain payload..."
    extract_payload "$rootfs_payload"
else
    build_native_payload
fi

if [[ -n "$rust_nightly_dir" ]]; then
    echo "overlaying explicit Rust nightly sysroot: $rust_nightly_dir"
    rm -rf "$overlay_dir/opt/rust-nightly"
    mkdir -p "$overlay_dir/opt/rust-nightly"
    (cd "$rust_nightly_dir" && tar cf - .) | (cd "$overlay_dir/opt/rust-nightly" && tar xf -)
    cat >"$overlay_dir/opt/rustc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustc --sysroot /opt/rust-nightly "$@"
WRAP
    cat >"$overlay_dir/opt/rustdoc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /lib/ld-musl-aarch64.so.1 --library-path /opt/rust-nightly/lib:/usr/lib /opt/rust-nightly/bin/rustdoc --sysroot /opt/rust-nightly "$@"
WRAP
    chmod +x "$overlay_dir/opt/rustc-nightly-sysroot" "$overlay_dir/opt/rustdoc-nightly-sysroot"
fi

echo "injecting toolchain payload with debugfs..."
: >"$debugfs_cmd"
while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    [[ "$rel" = "$path" ]] && continue
    printf 'mkdir /%s\n' "$rel" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type d | sort)

while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    guest_path="/$rel"
    printf 'rm %s\n' "$guest_path" >>"$debugfs_cmd"
    printf 'write %s %s\n' "$path" "$guest_path" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type f | sort)

while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    guest_path="/$rel"
    target="$(readlink "$path")"
    printf 'rm %s\n' "$guest_path" >>"$debugfs_cmd"
    printf 'symlink %s %s\n' "$guest_path" "$target" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type l | sort)

if ! "$debugfs" -w -f "$debugfs_cmd" "$toolchain_rootfs" >"$debugfs_log" 2>&1; then
    cat "$debugfs_log" >&2
    exit 1
fi

echo "injecting TGOSKits source into self-build rootfs..."
"$script_dir/prepare_rootfs.sh" \
    --base-rootfs "$toolchain_rootfs" \
    --output-rootfs "$selfbuild_rootfs" \
    --source "$source_dir"

echo "rootfs build complete"
echo "toolchain_rootfs=$toolchain_rootfs"
echo "selfbuild_rootfs=$selfbuild_rootfs"
