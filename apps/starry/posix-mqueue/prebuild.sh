#!/usr/bin/env bash
# Cross-compile the POSIX message queue carpet into the app overlay.
#
# Vehicle (per the delivery plan): the Open POSIX Test Suite mq_* conformance
# cases (self-contained, one main() each) plus a deterministic self-written
# carpet. Everything is built as a static musl binary on the host and dropped
# into the overlay so the on-target runner can execute it without a compiler.
#
# LTP mq_* (mq_open01, mq_notify0[1-3], ...) is intentionally NOT bundled: it
# depends on the libltp runtime (tst_test harness, SAFE_* wrappers, needs_root
# / nobody-user assumptions) whose cross build is not reproducible here. The
# Open POSIX conformance suite is the standards-compliance authority used
# instead; see README.md.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

[[ -n "$overlay_dir" ]] || { echo "error: STARRY_OVERLAY_DIR is required" >&2; exit 1; }

case "$STARRY_ARCH" in
    aarch64)     MUSL_TARGET="aarch64-linux-musl" ;;
    riscv64)     MUSL_TARGET="riscv64-linux-musl" ;;
    x86_64)      MUSL_TARGET="x86_64-linux-musl" ;;
    loongarch64) MUSL_TARGET="loongarch64-linux-musl" ;;
    *) echo "ERROR: unsupported arch: $STARRY_ARCH" >&2; exit 1 ;;
esac

# Locate the cross gcc. The musl-cross toolchains live under a few well-known
# prefixes on this builder; accept whichever is present on PATH or in /opt.
CC=""
for cand in \
    "${MUSL_TARGET}-gcc" \
    "/opt/${MUSL_TARGET}-cross/bin/${MUSL_TARGET}-gcc" \
    "/opt/cross/${MUSL_TARGET}-cross/bin/${MUSL_TARGET}-gcc"; do
    if command -v "$cand" >/dev/null 2>&1; then CC="$cand"; break; fi
done
[[ -n "$CC" ]] || { echo "ERROR: no cross compiler for $MUSL_TARGET" >&2; exit 1; }
echo "Using CC=$CC"

prog_dir="$app_dir/programs"
dest="$overlay_dir/usr/bin/mqueue-tests"
mkdir -p "$dest"

CFLAGS="-static -O2 -Wall -std=gnu11"

# 1. Deterministic self-written carpet.
$CC $CFLAGS -o "$dest/mq_carpet" "$prog_dir/mq_carpet.c"
echo "built mq_carpet"

# 2. Open POSIX conformance mq_* cases. One binary per source file, named
#    op_<interface>_<case> so the runner can glob them (op_*).
#
# A compile failure is a hard error: the whole point of the prebuild is to
# ship every bundled case as a runnable binary, so a case that will not build
# must abort the overlay rather than silently disappear from the suite (which
# would let the on-target runner report a smaller-but-green PASS count). The
# compiler diagnostics are left on stderr instead of being swallowed by
# `2>/dev/null` so a genuine breakage is visible.
# The vendored suite carries exactly these 119 conformance cases, enumerated by
# path in openposix/MANIFEST (one "mq_<iface>/<case>.c" per line). The build is
# driven from that manifest and the on-disk glob is diffed against it, so any
# drift - a dropped, renamed, added, or swapped source that a bare count check
# would miss - aborts the overlay instead of letting the on-target runner report
# a smaller-but-green PASS count.
EXPECTED_OPENPOSIX=119
manifest="$prog_dir/openposix/MANIFEST"
[[ -f "$manifest" ]] || { echo "ERROR: missing manifest $manifest" >&2; exit 1; }
expected_list="$(LC_ALL=C sort -u "$manifest")"
actual_list="$(cd "$prog_dir/openposix" && ls mq_*/*.c | LC_ALL=C sort -u)"
if [[ "$expected_list" != "$actual_list" ]]; then
    echo "ERROR: openposix case set drifted from MANIFEST:" >&2
    diff <(printf '%s\n' "$expected_list") <(printf '%s\n' "$actual_list") >&2
    exit 1
fi
built=0
while IFS= read -r rel; do
    [[ -n "$rel" ]] || continue
    iface="$(dirname "$rel")"
    case_name="$(basename "$rel" .c)"
    out="op_${iface}_${case_name}"
    $CC $CFLAGS -I "$prog_dir/openposix" -o "$dest/$out" "$prog_dir/openposix/$rel"
    built=$((built + 1))
done <<< "$expected_list"
if [ "$built" -ne "$EXPECTED_OPENPOSIX" ]; then
    echo "ERROR: built $built Open POSIX conformance binaries, expected $EXPECTED_OPENPOSIX" >&2
    exit 1
fi
echo "built $built Open POSIX conformance binaries (exact MANIFEST match, $EXPECTED_OPENPOSIX cases)"

# 3. On-target runner + shell entry.
install -Dm0755 "$prog_dir/run-mq-tests.sh" "$overlay_dir/usr/bin/run-mq-tests.sh"

echo "posix-mqueue overlay ready under $dest"
