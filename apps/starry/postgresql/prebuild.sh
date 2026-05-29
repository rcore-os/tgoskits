#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
output_rootfs="${STARRY_OUTPUT_ROOTFS:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
staging_root="${STARRY_STAGING_ROOT:-}"

PG_VERSION="16.4"
PG_SRC="postgresql-${PG_VERSION}"
PG_TARBALL="${PG_SRC}.tar.bz2"
PG_URL="https://ftp.postgresql.org/pub/source/v${PG_VERSION}/${PG_TARBALL}"
PG_PREFIX="/usr/local/pgsql"
PG_BUILD_DIR=""
NPROC="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo 4)"

require_env() {
    local name="$1"; local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2; exit 1
    fi
}

ensure_host_tools() {
    local missing=()
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v tar >/dev/null 2>&1 || missing+=(tar)
    command -v wget >/dev/null 2>&1 || missing+=(wget)
    command -v make >/dev/null 2>&1 || missing+=(make)
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required host packages: ${missing[*]}" >&2; exit 1
    fi
}

resolve_cross_compiler() {
    case "$arch" in
        riscv64)   CC="riscv64-linux-musl-gcc"; AR="riscv64-linux-musl-ar"; HOST="riscv64-linux-musl" ;;
        aarch64)   CC="aarch64-linux-musl-gcc";  AR="aarch64-linux-musl-ar";  HOST="aarch64-linux-musl" ;;
        x86_64)    CC="x86_64-linux-musl-gcc";   AR="x86_64-linux-musl-ar";   HOST="x86_64-linux-musl" ;;
        loongarch64) CC="loongarch64-linux-musl-gcc"; AR="loongarch64-linux-musl-ar"; HOST="loongarch64-linux-musl" ;;
        *) echo "error: unsupported arch: $arch" >&2; exit 1 ;;
    esac
    if ! command -v "$CC" >/dev/null 2>&1; then echo "error: cross-compiler $CC not found" >&2; exit 1; fi
    if ! command -v "$AR" >/dev/null 2>&1; then echo "error: archiver $AR not found" >&2; exit 1; fi
}

rebuild_clean_rootfs_cache() {
    local archive="$1"
    echo "rebuilding clean rootfs cache: cargo xtask starry rootfs --arch $arch"
    rm -f "$archive" "$base_rootfs"
    (cd "$workspace" && cargo xtask starry rootfs --arch "$arch")
    if [[ ! -f "$archive" ]]; then
        echo "error: rootfs command did not produce clean archive: $archive" >&2; exit 1
    fi
}

refresh_output_rootfs() {
    local output_dir; output_dir="$(dirname "$output_rootfs")"
    mkdir -p "$output_dir"
    # If base image exists, copy directly (avoids 2x disk overhead of archive extraction)
    if [[ -f "$base_rootfs" ]]; then
        echo "refreshing PostgreSQL app rootfs from base image: $base_rootfs"
        cp "$base_rootfs" "$output_rootfs"
        chmod 0644 "$output_rootfs"
        return 0
    fi
    local base_dir image_name archive tmp_dir extracted
    base_dir="$(dirname "$base_rootfs")"
    image_name="$(basename "$base_rootfs")"
    archive="$base_dir/${image_name}.tar.xz"
    if [[ ! -f "$archive" ]]; then rebuild_clean_rootfs_cache "$archive"; fi
    echo "refreshing PostgreSQL app rootfs from clean archive: $archive"
    tmp_dir="$(mktemp -d "$output_dir/.postgresql-rootfs.XXXXXX")"
    ( trap 'rm -rf "$tmp_dir"' EXIT
        if ! tar -xJf "$archive" -C "$tmp_dir" "$image_name"; then
            rm -f "$archive"; rebuild_clean_rootfs_cache "$archive"
            tar -xJf "$archive" -C "$tmp_dir" "$image_name"
        fi
        extracted="$tmp_dir/$image_name"
        if [[ ! -s "$extracted" ]]; then echo "error: clean rootfs archive did not produce $image_name" >&2; exit 1; fi
        chmod 0644 "$extracted"; mv -f "$extracted" "$output_rootfs"
    )
}

build_postgresql() {
    local build_root="$workspace/tmp/axbuild/starry-app/postgresql/build-$arch"
    PG_BUILD_DIR="$build_root"
    if [[ -f "$build_root/.build-done" ]]; then
        echo "PostgreSQL already built for $arch, skipping"; return 0
    fi
    mkdir -p "$build_root"; rm -rf "$build_root/$PG_SRC"
    echo "downloading PostgreSQL ${PG_VERSION} source..."
    local tarball_path="$build_root/$PG_TARBALL"
    if [[ ! -f "$tarball_path" ]]; then wget -q --show-progress -O "$tarball_path" "$PG_URL"; fi
    echo "extracting PostgreSQL source..."
    tar -xjf "$tarball_path" -C "$build_root"
    export CC="$CC" AR="$AR"
    ( cd "$build_root/$PG_SRC"
        echo "configuring PostgreSQL for $HOST..."
        ./configure --host="$HOST" --without-readline --without-zlib --without-openssl \
            --without-icu --disable-thread-safety CFLAGS="-Os" LDFLAGS="" >"$build_root/configure.log" 2>&1
        # --export-dynamic needed for dlopen of extension modules (plpgsql)
        if sed --version 2>/dev/null | grep -q GNU; then
            sed -i 's/^LDFLAGS_EX = $/LDFLAGS_EX = -Wl,--export-dynamic/' src/Makefile.global
        else
            sed -i '' 's/^LDFLAGS_EX = $/LDFLAGS_EX = -Wl,--export-dynamic/' src/Makefile.global
        fi
        echo "generating derived headers..."
        make -j1 -C src/include >>"$build_root/build.log" 2>&1
        echo "building PostgreSQL backend..."
        make -j"$NPROC" -C src/backend >>"$build_root/build.log" 2>&1
        echo "building libpq..."
        make -j"$NPROC" -C src/interfaces/libpq >>"$build_root/build.log" 2>&1
        echo "building initdb..."
        make -j"$NPROC" -C src/bin/initdb >>"$build_root/build.log" 2>&1
        echo "building psql..."
        make -j"$NPROC" -C src/bin/psql >>"$build_root/build.log" 2>&1
        echo "building pg_ctl..."
        make -j"$NPROC" -C src/bin/pg_ctl >>"$build_root/build.log" 2>&1
        echo "building plpgsql..."
        make -j"$NPROC" -C src/pl/plpgsql >>"$build_root/build.log" 2>&1
    )
    touch "$build_root/.build-done"
    echo "PostgreSQL build complete for $arch"
}

install_to_staging() {
    local install_root="$PG_BUILD_DIR/install"
    if [[ -f "$install_root/.install-done" ]]; then
        echo "PostgreSQL already installed to staging, skipping"
    else
        echo "installing PostgreSQL to staging..."
        mkdir -p "$install_root"
        ( cd "$PG_BUILD_DIR/$PG_SRC"
            make DESTDIR="$install_root" -C src/backend install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/interfaces/libpq install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/bin/initdb install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/bin/psql install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/bin/pg_ctl install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/pl/plpgsql install >>"$PG_BUILD_DIR/install.log" 2>&1
            make DESTDIR="$install_root" -C src/include install >>"$PG_BUILD_DIR/install.log" 2>&1
            # Install share data (SQL files, timezone data) that component-level installs miss
            for subdir in snowball timezone; do
                if [[ -d "src/backend/$subdir" ]]; then
                    make DESTDIR="$install_root" -C "src/backend/$subdir" install >>"$PG_BUILD_DIR/install.log" 2>&1 || true
                fi
            done
            mkdir -p "$install_root/$PG_PREFIX/share"
            find src/backend -name '*.sql' -o -name '*.dat' | while read -r f; do
                cp "$f" "$install_root/$PG_PREFIX/share/" 2>/dev/null || true
            done
        )
        touch "$install_root/.install-done"
    fi
    echo "copying PostgreSQL to staging root..."
    mkdir -p "$staging_root/$PG_PREFIX"
    cp -r "$install_root/$PG_PREFIX/"* "$staging_root/$PG_PREFIX/"
}

find_pg_library() {
    local library="$1"; local dir
    for dir in lib usr/lib usr/local/lib usr/local/pgsql/lib; do
        if [[ -e "$staging_root/$dir/$library" ]]; then printf '/%s/%s\n' "$dir" "$library"; return 0; fi
    done
    return 1
}

copy_runtime_dependencies() {
    local pending=("$@"); local seen=" "; local guest_path library
    while [[ ${#pending[@]} -gt 0 ]]; do
        guest_path="${pending[0]}"; pending=("${pending[@]:1}")
        if [[ "$seen" == *" $guest_path "* ]]; then continue; fi
        seen+="$guest_path "
        local full_source="$staging_root$guest_path"
        if [[ ! -f "$full_source" ]]; then echo "warning: missing dep: $full_source" >&2; continue; fi
        mkdir -p "$(dirname "$overlay_dir$guest_path")"
        cp "$full_source" "$overlay_dir$guest_path"
        chmod 0755 "$overlay_dir$guest_path"
        while IFS= read -r library; do
            local library_path
            if ! library_path="$(find_pg_library "$library")"; then continue; fi
            pending+=("$library_path")
        done < <(readelf -d "$full_source" 2>/dev/null | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p' || true)
    done
}

populate_overlay() {
    local pg_binaries=("$PG_PREFIX/bin/postgres" "$PG_PREFIX/bin/initdb" "$PG_PREFIX/bin/psql" "$PG_PREFIX/bin/pg_ctl")
    for bin in "${pg_binaries[@]}"; do
        local src="$staging_root$bin"; local dest="$overlay_dir$bin"
        if [[ -f "$src" ]]; then
            mkdir -p "$(dirname "$dest")"; cp "$src" "$dest"; chmod 0755 "$dest"
        else echo "error: missing PostgreSQL binary: $src" >&2; exit 1; fi
    done
    # Copy libraries
    local lib_dir="$staging_root/$PG_PREFIX/lib"
    if [[ -d "$lib_dir" ]]; then mkdir -p "$overlay_dir/$PG_PREFIX/lib"; cp -r "$lib_dir/"* "$overlay_dir/$PG_PREFIX/lib/"; fi
    # Copy share data
    local share_dir="$staging_root/$PG_PREFIX/share"
    if [[ -d "$share_dir" ]]; then mkdir -p "$overlay_dir/$PG_PREFIX/share"; cp -r "$share_dir/"* "$overlay_dir/$PG_PREFIX/share/"; fi
    # Runtime library deps
    copy_runtime_dependencies "${pg_binaries[@]}"
    # Test script
    mkdir -p "$overlay_dir/usr/bin"
    cp "$app_dir/postgresql-test.sh" "$overlay_dir/usr/bin/postgresql-test.sh"
    chmod 0755 "$overlay_dir/usr/bin/postgresql-test.sh"
    # Musl library search path
    mkdir -p "$overlay_dir/etc"
    echo '/lib:/usr/lib:/usr/local/lib:/usr/local/pgsql/lib' > "$overlay_dir/etc/ld-musl-${arch}.path"
}

check_postgres_binaries() {
    for bin in "$PG_PREFIX/bin/postgres" "$PG_PREFIX/bin/initdb" "$PG_PREFIX/bin/psql"; do
        if [[ ! -f "$staging_root$bin" ]]; then echo "error: PostgreSQL binary missing: $bin" >&2; return 1; fi
    done
    echo "all PostgreSQL binaries present for $arch"
}

require_env STARRY_ARCH "$arch"
require_env STARRY_BASE_ROOTFS "$base_rootfs"
require_env STARRY_OUTPUT_ROOTFS "$output_rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"
require_env STARRY_STAGING_ROOT "$staging_root"

ensure_host_tools
resolve_cross_compiler
refresh_output_rootfs
build_postgresql
install_to_staging
check_postgres_binaries
populate_overlay
echo "PostgreSQL app prebuild complete for $arch"
