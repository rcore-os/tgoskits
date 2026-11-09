#!/usr/bin/env bash
set -euo pipefail

# rustc treats os="uefi" as Windows-like and injects --nxcompat into the
# gnu-lld command line. GNU ELF lld rejects that COFF-only option. Keep every
# other linker argument and the real linker's exit status unchanged.
if [[ -n "${AXLOADER_RUST_LLD:-}" ]]; then
    real_linker=$AXLOADER_RUST_LLD
else
    target_libdir=$("${RUSTC:-rustc}" --print target-libdir)
    real_linker="$(dirname "$target_libdir")/bin/rust-lld"
fi

if [[ ! -x "$real_linker" ]]; then
    echo "loongarch64 ELF linker adapter: rust-lld is not executable: $real_linker" >&2
    exit 127
fi

args=()
for arg in "$@"; do
    if [[ "$arg" != "--nxcompat" ]]; then
        args+=("$arg")
    fi
done

exec "$real_linker" "${args[@]}"
