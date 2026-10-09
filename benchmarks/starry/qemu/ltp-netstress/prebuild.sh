#!/usr/bin/env bash
set -euo pipefail

: "${STARRY_APP_DIR:?}"
: "${STARRY_OVERLAY_DIR:?}"
: "${STARRY_WORKSPACE:?}"
: "${STARRY_ARCH:?}"
[[ "$STARRY_ARCH" == x86_64 ]] || { echo 'netstress app currently supports x86_64' >&2; exit 1; }

version=20260529
sha256=685d83c6e370ac09201fb79593412f868fe031ee2890e204b5727fedcf51fb47
build_dir="$STARRY_WORKSPACE/target/ltp-netstress"
mkdir -p "$build_dir"
archive="$build_dir/ltp-full-$version.tar.xz"
if [[ ! -f "$archive" ]]; then
    curl --fail --location --output "$archive.part" \
        "https://github.com/linux-test-project/ltp/releases/download/$version/ltp-full-$version.tar.xz"
    printf '%s  %s\n' "$sha256" "$archive.part" | sha256sum -c -
    mv "$archive.part" "$archive"
fi
printf '%s  %s\n' "$sha256" "$archive" | sha256sum -c -

# Build only the upstream network workload and its own libltp dependency.
# Static musl avoids coupling the app to the guest's shared-library version.
build_with_docker() {
    docker run --rm --platform linux/amd64 \
        -e "BUILD_UID=$(id -u)" -e "BUILD_GID=$(id -g)" \
        -v "$build_dir:/build" -w /build alpine:3.23 sh -ec '
            trap '\''chown -R "$BUILD_UID:$BUILD_GID" /build'\'' EXIT
            apk add --no-cache build-base linux-headers pkgconf xz
            tar -xf ltp-full-20260529.tar.xz
            cd ltp-full-20260529
            ./configure --without-numa --without-tirpc --without-modules LDFLAGS=-static
            make -j2 -C testcases/network/netstress
            cp testcases/network/netstress/netstress /build/netstress
        '
}

# Fallback for hosts without a usable docker daemon (self-hosted CI runners):
# build statically with the host toolchain. The static link keeps the binary
# usable in the musl guest rootfs, just like the docker path.
build_native() {
    command -v make >/dev/null 2>&1 || { echo 'error: netstress native build requires make' >&2; exit 1; }
    for cc in x86_64-linux-musl-gcc musl-gcc cc gcc; do
        if command -v "$cc" >/dev/null 2>&1; then
            export CC="$cc"
            break
        fi
    done
    [ -n "${CC:-}" ] || { echo 'error: no C compiler for netstress native build' >&2; exit 1; }
    rm -rf "$build_dir/ltp-full-$version"
    tar -xf "$archive" -C "$build_dir"
    cd "$build_dir/ltp-full-$version"
    ./configure --without-numa --without-tirpc --without-modules LDFLAGS=-static
    make -j2 -C testcases/network/netstress
    cp testcases/network/netstress/netstress "$build_dir/netstress"
}

if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    build_with_docker
else
    echo "==> docker unavailable, building netstress with the host toolchain"
    build_native
fi
install -D -m 0755 "$build_dir/netstress" \
    "$STARRY_OVERLAY_DIR/usr/bin/ltp-netstress"
mkdir -p "$STARRY_OVERLAY_DIR/usr/share/ltp-netstress"
printf '%s\n' "$version" >"$STARRY_OVERLAY_DIR/usr/share/ltp-netstress/Version"
install -D -m 0755 "$STARRY_APP_DIR/ltp-netstress.sh" \
    "$STARRY_OVERLAY_DIR/usr/bin/ltp-netstress-run"
