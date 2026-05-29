#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
asset_dir="$workspace/target/jcode/assets"
cache_dir="$workspace/target/jcode/cache"

ALPINE_VERSION="3.21"
ALPINE_PATCH="3.21.3"
ALPINE_MINIROOTFS_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VERSION}/releases/x86_64/alpine-minirootfs-${ALPINE_PATCH}-x86_64.tar.gz"
ALPINE_MINIROOTFS_SHA256="1a694899e406ce55d32334c47ac0b2efb6c06d7e878102d1840892ad44cd5239"

JCODE_VERSION="0.12.0"
JCODE_URL="https://github.com/1jehuang/jcode/releases/download/v${JCODE_VERSION}/jcode-linux-x86_64.tar.gz"
JCODE_SHA256="cc7fef26c348124af40db1793481b46945842e20a7b6c684fc66bea7b2524f0b"

cleanup() {
    rm -rf "${TMPDIR:-/tmp}/starry-jcode-assets.$$" 2>/dev/null || true
}
trap cleanup EXIT

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        echo "       install with: apt-get install -y $2" >&2
        exit 1
    fi
}

need_cmd curl curl
need_cmd tar tar
need_cmd install coreutils
need_cmd patchelf patchelf
need_cmd as binutils
need_cmd ld binutils
need_cmd qemu-x86_64-static qemu-user-static
need_cmd sha256sum coreutils

verify_sha256() {
    local expected="$1" path="$2"
    local actual
    actual="$(sha256sum "$path" | awk '{print $1}')"
    if [[ "$actual" != "$expected" ]]; then
        echo "error: SHA-256 mismatch for $path" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

mkdir -p "$cache_dir"

# Download jcode release tarball (cached).
jcode_tgz="$cache_dir/jcode-linux-x86_64.tar.gz"
if [[ ! -s "$jcode_tgz" ]]; then
    echo "Downloading jcode v${JCODE_VERSION} from $JCODE_URL ..."
    curl -fSL --retry 5 --retry-delay 3 --retry-all-errors --max-time 300 \
        "$JCODE_URL" -o "$jcode_tgz"
fi
verify_sha256 "$JCODE_SHA256" "$jcode_tgz"

TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/starry-jcode-assets.$$.XXXXX")"
tar xzf "$jcode_tgz" -C "$TMPDIR"

# Extract Alpine minirootfs as staging root.
alpine_tgz="$cache_dir/alpine-minirootfs-${ALPINE_PATCH}-x86_64.tar.gz"
if [[ ! -s "$alpine_tgz" ]]; then
    echo "Downloading Alpine minirootfs ${ALPINE_VERSION} ..."
    curl -fSL --retry 5 --retry-delay 3 --retry-all-errors --max-time 120 \
        "$ALPINE_MINIROOTFS_URL" -o "$alpine_tgz"
fi
verify_sha256 "$ALPINE_MINIROOTFS_SHA256" "$alpine_tgz"

staging="$TMPDIR/staging"
mkdir -p "$staging"
tar xzf "$alpine_tgz" -C "$staging"

# Install packages into staging root via qemu-user + apk.
# Unset LD_LIBRARY_PATH to prevent host tools from loading the staging musl libc.
(
    unset LD_LIBRARY_PATH
    MUSL_LD="$staging/lib/ld-musl-x86_64.so.1"
    MUSL_LIBPATH="$staging/lib:$staging/usr/lib"
    "$MUSL_LD" --library-path "$MUSL_LIBPATH" \
        "$staging/sbin/apk" \
        --root "$staging" --initdb --allow-untrusted --no-progress --no-scripts \
        add patchelf krb5-libs
)

# Copy jcode binary into staging.
jcode_bin_name="jcode-linux-x86_64.bin"
if [[ ! -f "$TMPDIR/$jcode_bin_name" ]]; then
    jcode_bin_name="$(ls "$TMPDIR"/*.bin 2>/dev/null | head -1)"
fi
[[ -f "$TMPDIR/$jcode_bin_name" ]] || { echo "error: jcode binary not found in tarball" >&2; exit 1; }
mkdir -p "$staging/usr/lib/jcode"
install -m 0755 "$TMPDIR/$jcode_bin_name" "$staging/usr/lib/jcode/jcode.bin"

# Copy bundled SSL libs.
for f in "$TMPDIR"/libssl.so* "$TMPDIR"/libcrypto.so*; do
    [[ -f "$f" ]] && cp "$f" "$staging/usr/lib/jcode/"
done

# ── Patch jcode.bin via staging root's patchelf ─────────────────────────────
PATCHELF="$staging/usr/bin/patchelf"
QEMU_USER="/usr/bin/qemu-x86_64-static"
MUSL_INTERP="/lib/ld-musl-x86_64.so.1"
MUSL_LIBC="libc.musl-x86_64.so.1"
GLIBC_INTERP="ld-linux-x86-64.so.2"

run_patchelf() {
    "$QEMU_USER" -L "$staging" "$PATCHELF" "$@"
}

run_patchelf \
    --set-interpreter "$MUSL_INTERP" \
    --set-rpath "/usr/lib/jcode" \
    --remove-needed "$GLIBC_INTERP" \
    --replace-needed "libc.so.6" "$MUSL_LIBC" \
    --remove-needed "libm.so.6" \
    --remove-needed "libdl.so.2" \
    --remove-needed "librt.so.1" \
    --remove-needed "libpthread.so.0" \
    "$staging/usr/lib/jcode/jcode.bin"

# Patch bundled libssl and libcrypto.
for LIBSSL in "$staging/usr/lib/jcode"/libssl.so*; do
    [[ -f "$LIBSSL" ]] || continue
    run_patchelf \
        --set-rpath "/usr/lib/jcode:/usr/lib" \
        --replace-needed "libc.so.6" "$MUSL_LIBC" \
        --remove-needed "libdl.so.2" \
        "$LIBSSL"
done
for LIBCRYPTO in "$staging/usr/lib/jcode"/libcrypto.so*; do
    [[ -f "$LIBCRYPTO" ]] || continue
    run_patchelf \
        --set-rpath "/usr/lib/jcode:/usr/lib" \
        --replace-needed "libc.so.6" "$MUSL_LIBC" \
        --remove-needed "libdl.so.2" \
        "$LIBCRYPTO"
done

# ── Build glibc stub shared library ──────────────────────────────────────────
# jcode.bin references glibc-specific symbols that don't exist in musl.
# Build a tiny .so that stubs them out.
STUB_ASM="$TMPDIR/glibc_stub.s"
STUB_OBJ="$TMPDIR/glibc_stub.o"
STUB_SO="$staging/usr/lib/jcode/libglibc_stub.so"

/usr/bin/printf '%s\n' \
    '.text' \
    '.globl mallopt' \
    '.type mallopt, @function' \
    'mallopt: mov $1, %eax; ret' \
    '' \
    '.globl malloc_trim' \
    '.type malloc_trim, @function' \
    'malloc_trim: xor %eax, %eax; ret' \
    '' \
    '.globl __res_init' \
    '.type __res_init, @function' \
    '__res_init: xor %eax, %eax; ret' \
    '' \
    '.globl res_init' \
    '.type res_init, @function' \
    'res_init: xor %eax, %eax; ret' \
    '' \
    '.globl __register_atfork' \
    '.type __register_atfork, @function' \
    '__register_atfork: xor %eax, %eax; ret' \
    '' \
    '.globl gnu_get_libc_version' \
    '.type gnu_get_libc_version, @function' \
    'gnu_get_libc_version: lea .Lglibc_ver(%rip), %rax; ret' \
    '.section .rodata' \
    '.Lglibc_ver: .asciz "2.17"' \
    '' \
    '.text' \
    '.globl sdallocx' \
    '.type sdallocx, @function' \
    'sdallocx: jmp free@PLT' \
    '' \
    '.globl __fprintf_chk' \
    '.type __fprintf_chk, @function' \
    '__fprintf_chk: mov %rdx, %rsi; mov %rcx, %rdx; jmp fprintf@PLT' \
    '' \
    '.globl __printf_chk' \
    '.type __printf_chk, @function' \
    '__printf_chk: mov %rsi, %rdi; mov %rdx, %rsi; jmp printf@PLT' \
    '' \
    '.globl __vfprintf_chk' \
    '.type __vfprintf_chk, @function' \
    '__vfprintf_chk: mov %rdx, %rsi; mov %rcx, %rdx; jmp vfprintf@PLT' \
    '' \
    '.globl __sprintf_chk' \
    '.type __sprintf_chk, @function' \
    '__sprintf_chk: mov %rdx, %rsi; mov %rcx, %rdx; jmp sprintf@PLT' \
    '' \
    '.globl __memcpy_chk' \
    '.type __memcpy_chk, @function' \
    '__memcpy_chk: jmp memcpy@PLT' \
    '' \
    '.globl __memset_chk' \
    '.type __memset_chk, @function' \
    '__memset_chk: jmp memset@PLT' \
    '' \
    '.globl __strcat_chk' \
    '.type __strcat_chk, @function' \
    '__strcat_chk: jmp strcat@PLT' \
    '' \
    '.globl __fread_chk' \
    '.type __fread_chk, @function' \
    '__fread_chk: mov %rdx, %rax; mov %rcx, %rdx; mov %r8, %rcx; mov %rax, %rsi; jmp fread@PLT' \
    '' \
    '.globl __syslog_chk' \
    '.type __syslog_chk, @function' \
    '__syslog_chk: mov %rdx, %rsi; mov %rcx, %rdx; jmp syslog@PLT' \
    > "$STUB_ASM"

# Assemble with host 'as', link with host 'ld'.
as -o "$STUB_OBJ" "$STUB_ASM"
ld -shared -L "$staging/usr/lib" -L "$staging/lib" -lc -o "$STUB_SO" "$STUB_OBJ"
chmod 755 "$STUB_SO"

# Replace glibc NEEDED with musl so the stub loads in the musl guest.
patchelf --replace-needed "libc.so.6" "$MUSL_LIBC" "$STUB_SO"

# Inject stub as NEEDED into jcode.bin (must come before libc).
run_patchelf --add-needed "libglibc_stub.so" "$staging/usr/lib/jcode/jcode.bin"

# Create launcher script.
LAUNCHER="$TMPDIR/jcode"
printf '#!/bin/sh\nexec /usr/lib/jcode/jcode.bin "$@"\n' > "$LAUNCHER"
chmod 755 "$LAUNCHER"

# Collect assets.
rm -rf "$asset_dir"
mkdir -p "$asset_dir"
install -m 0755 "$staging/usr/lib/jcode/jcode.bin" "$asset_dir/jcode.bin"
install -m 0755 "$staging/usr/lib/jcode/libglibc_stub.so" "$asset_dir/libglibc_stub.so"
install -m 0755 "$LAUNCHER" "$asset_dir/jcode"
for f in "$staging/usr/lib/jcode"/libssl.so* "$staging/usr/lib/jcode"/libcrypto.so*; do
    [[ -f "$f" ]] && install -m 0755 "$f" "$asset_dir/"
done
for lib in libcom_err.so.2 libgssapi_krb5.so.2 libk5crypto.so.3 \
           libkeyutils.so.1 libkrb5.so.3 libkrb5support.so.0; do
    [[ -f "$staging/usr/lib/$lib" ]] && install -m 0755 "$staging/usr/lib/$lib" "$asset_dir/"
done

# Verify.
for f in jcode.bin libglibc_stub.so jcode; do
    [[ -f "$asset_dir/$f" ]] || { echo "error: missing asset: $f" >&2; exit 1; }
done

echo "jcode assets ready in $asset_dir"
