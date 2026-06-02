#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi
if [[ -z "$base_rootfs" ]]; then
    echo "error: STARRY_BASE_ROOTFS is required" >&2
    exit 1
fi

case "$STARRY_ARCH" in
    aarch64)
        MUSL_TARGET="aarch64-linux-musl"
        MUSL_ARCH="aarch64"
        ;;
    riscv64)
        MUSL_TARGET="riscv64-linux-musl"
        MUSL_ARCH="riscv64"
        ;;
    x86_64)
        MUSL_TARGET="x86_64-linux-musl"
        MUSL_ARCH="x86_64"
        ;;
    *)
        echo "ERROR: unsupported arch: $STARRY_ARCH" >&2
        exit 1
        ;;
esac

command -v debugfs >/dev/null 2>&1 || { echo "ERROR: debugfs not found" >&2; exit 1; }

# Resolve the lld driver on PATH. Debian/Ubuntu's llvm-14 package installs the
# linker as 'ld.lld', while Arch/Alpine symlink the wrapper as 'lld'. Accept
# either so a system that has clang+ld.lld does not silently fall back to
# musl-cross GCC, whose default GNU ld cannot consume the .relr.dyn section
# shipped in current Alpine libc.so.
lld_linker=""
if command -v lld >/dev/null 2>&1; then
    lld_linker="lld"
elif command -v ld.lld >/dev/null 2>&1; then
    lld_linker="ld.lld"
fi

# Build into a temp directory so the compiled ELF never lands inside the
# application source tree. The trap cleans both sysroot and build_dir on exit.
sysroot="$(mktemp -d)"
build_dir="$(mktemp -d)"
trap 'rm -rf "$sysroot" "$build_dir"' EXIT
debugfs -R "rdump / $sysroot" "$base_rootfs" >/dev/null 2>&1

if command -v clang >/dev/null 2>&1 && [[ -n "$lld_linker" ]]; then
    CC="clang"
    CC_FLAGS="--target=$MUSL_TARGET --sysroot=$sysroot -isystem $sysroot/usr/include -fuse-ld=lld -nostdlib -Wl,--strip-debug"
elif command -v "${MUSL_TARGET}-gcc" >/dev/null 2>&1; then
    CC="${MUSL_TARGET}-gcc"
    if [[ -n "$lld_linker" ]]; then
        # -nostdlib disables the toolchain's default CRT (crt1.o/crti.o/...)
        # so the script can supply the Alpine musl Scrt1.o/crti.o/crtn.o
        # trio by hand without producing _start/_init/_fini duplicates.
        # -fuse-ld=lld routes the link through the resolved lld driver,
        # which (unlike GNU ld) handles the .relr.dyn section in current
        # Alpine libc.so.
        CC_FLAGS="--sysroot=$sysroot -nostdlib -fuse-ld=lld"
    else
        echo "ERROR: ${MUSL_TARGET}-gcc found, but no lld driver (lld or ld.lld) on PATH." >&2
        echo "  The Alpine rootfs in this smoke case uses .relr.dyn, which GNU ld cannot consume." >&2
        echo "  Install lld/lld.lld (e.g. 'apt-get install lld-14') and retry." >&2
        exit 1
    fi
else
    echo "ERROR: no compiler for $MUSL_TARGET (tried clang+lld/lld.lld, ${MUSL_TARGET}-gcc)" >&2
    exit 1
fi

$CC $CC_FLAGS \
    -L"$sysroot/usr/lib" \
    -Wl,--library-path="$sysroot/usr/lib" \
    "$sysroot/usr/lib/Scrt1.o" \
    "$sysroot/usr/lib/crti.o" \
    -lc \
    "$sysroot/usr/lib/crtn.o" \
    -o "$build_dir/dynamic-test" \
    "$app_dir/dynamic-test.c"

INTERP=$(readelf -l "$build_dir/dynamic-test" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
echo "INTERP path: $INTERP"
[[ -n "$INTERP" ]] || { echo "ERROR: no PT_INTERP found" >&2; exit 1; }

INTERP_BASENAME=$(basename "$INTERP")
MUSL_LD="$sysroot/lib/$INTERP_BASENAME"
if [[ ! -f "$MUSL_LD" ]]; then
    MUSL_LD="$sysroot/lib/libc.musl-$MUSL_ARCH.so.1"
fi
[[ -f "$MUSL_LD" ]] || { echo "ERROR: musl ld not found" >&2; exit 1; }

install -Dm0755 "$build_dir/dynamic-test" "$overlay_dir/usr/bin/dynamic-test"
install -Dm0755 "$MUSL_LD" "$overlay_dir/$INTERP"
install -Dm0755 "$app_dir/dynamic-test.sh" "$overlay_dir/usr/bin/dynamic-test.sh"
