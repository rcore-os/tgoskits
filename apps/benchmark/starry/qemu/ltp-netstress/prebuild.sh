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
install -D -m 0755 "$build_dir/netstress" \
    "$STARRY_OVERLAY_DIR/usr/bin/ltp-netstress"
mkdir -p "$STARRY_OVERLAY_DIR/usr/share/ltp-netstress"
printf '%s\n' "$version" >"$STARRY_OVERLAY_DIR/usr/share/ltp-netstress/Version"
install -D -m 0755 "$STARRY_APP_DIR/ltp-netstress.sh" \
    "$STARRY_OVERLAY_DIR/usr/bin/ltp-netstress-run"
