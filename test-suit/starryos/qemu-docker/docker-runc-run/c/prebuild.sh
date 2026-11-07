#!/bin/sh
# Downloads the prebuilt runc/busybox artifacts for the docker-runc-run case
# into the staging root. Runs inside the Alpine toolchain staging sysroot
# under qemu-user; `apk` and `curl` resolve through the axbuild guest command
# wrappers (see scripts/axbuild/src/test/build/wrappers.rs).
set -eu

STAGING="${STARRY_STAGING_ROOT:?}"
RUNC_VERSION="1.1.15"
# sha256 of the upstream release asset, from the signed runc.sha256sum.
RUNC_SHA256="c680f8c470ffb228944ca80e1a4dbb6768b3ad97057350852e128847f9dd10bc"
RUNC_URL="${STARRY_RUNC_URL:-https://github.com/opencontainers/runc/releases/download/v${RUNC_VERSION}/runc.arm64}"

# retry <attempts> <command...>
retry() {
    attempts="$1"
    shift
    n=1
    while ! "$@"; do
        if [ "$n" -ge "$attempts" ]; then
            echo "prebuild: `$1` failed after $n attempts" >&2
            return 1
        fi
        sleep $((n * 5))
        n=$((n + 1))
    done
}

# busybox-static is signature-verified by apk; curl is needed for runc.
retry 3 apk add busybox-static curl
[ -x "$STAGING/bin/busybox.static" ] || {
    echo "prebuild: busybox.static missing after apk add" >&2
    exit 1
}

# runc is a static Go binary from the signed upstream release.
retry 3 curl -fsSL --retry 2 -o "$STAGING/usr/bin/runc" "$RUNC_URL"
chmod 0755 "$STAGING/usr/bin/runc"
echo "$RUNC_SHA256  $STAGING/usr/bin/runc" | sha256sum -c -

# Verify the downloaded runc. Two supported preparation modes exist:
#
# - the system-QEMU fallback boots an aarch64 Linux guest, so the arm64 runc
#   executes natively there — try that first;
# - under qemu-user on a foreign-arch host the direct execve fails with
#   ENOEXEC, so fall back to qemu-aarch64[-static] (the same candidate order
#   axbuild uses for qemu-user selection).
if "$STAGING/usr/bin/runc" --version >/dev/null 2>&1; then
    "$STAGING/usr/bin/runc" --version
else
    QEMU_AARCH64=
    for candidate in qemu-aarch64-static qemu-aarch64; do
        if command -v "$candidate" >/dev/null 2>&1; then
            QEMU_AARCH64="$candidate"
            break
        fi
    done
    [ -n "$QEMU_AARCH64" ] || {
        echo "prebuild: runc did not run natively and no qemu-aarch64 runner is on PATH" >&2
        exit 1
    }
    "$QEMU_AARCH64" -L "$STAGING" "$STAGING/usr/bin/runc" --version || {
        echo "prebuild: downloaded runc is not executable" >&2
        exit 1
    }
fi
