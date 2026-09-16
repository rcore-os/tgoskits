#!/usr/bin/env bash
# Shared by the application prebuild and regression build.rs entry points.
# stdout is reserved for the resulting binary path; diagnostics go to stderr.
set -euo pipefail

CLAW_REPO="https://github.com/MuZhao2333/claw-code"
# Update this pin only together with a review of rust/Cargo.lock and build inputs.
CLAW_REV="af481e2cc92bf1f136d4c4a129ee98b75a3b7372"
TARGET="x86_64-unknown-linux-musl"
CACHE_DIR="${CLAW_CACHE_DIR:-${HOME}/.cache/claw-code-build}"
mkdir -p "$CACHE_DIR"
CACHE_DIR="$(cd "$CACHE_DIR" && pwd)"
# Never reuse the old unversioned repo, target directory or binary.
ARTIFACT_DIR="$CACHE_DIR/v1/$CLAW_REV/$TARGET"
CLAW_BIN="$ARTIFACT_DIR/claw"

# Serialize cache validation/publication across app and build.rs invocations.
exec 9>"$CACHE_DIR/.build.lock"
flock 9

provenance() {
    printf 'repository=%s\nrevision=%s\ntarget=%s\n' "$CLAW_REPO" "$CLAW_REV" "$TARGET"
    printf 'build=cargo build --workspace --release --locked\n'
    printf 'binary_sha256=%s\n' "$(sha256sum "$CLAW_BIN" | cut -d ' ' -f 1)"
}

if [[ -x "$CLAW_BIN" && -f "$CLAW_BIN.provenance" ]] &&
    cmp -s "$CLAW_BIN.provenance" <(provenance); then
    echo "claw binary verified at $CLAW_BIN" >&2
    printf '%s\n' "$CLAW_BIN"
    exit 0
fi

# A fresh checkout prevents stale or modified cached sources from being built.
BUILD_DIR="$(mktemp -d "$CACHE_DIR/.build-XXXXXX")"
trap 'rm -rf "$BUILD_DIR"' EXIT
(
    git init -q "$BUILD_DIR/repo"
    cd "$BUILD_DIR/repo"
    git fetch --no-tags --depth 1 "$CLAW_REPO" "$CLAW_REV"
    git checkout --detach FETCH_HEAD
    if [[ "$(git rev-parse HEAD)" != "$CLAW_REV" ]]; then
        echo "error: claw checkout does not match pinned revision $CLAW_REV"
        exit 1
    fi
    git ls-files --error-unmatch rust/Cargo.lock >/dev/null
    test -s rust/Cargo.lock
    rustup target add "$TARGET"
    cd rust
    cargo build --workspace --release --locked --target "$TARGET" --target-dir "$BUILD_DIR/target"
) >&2

mkdir -p "$ARTIFACT_DIR"
install -m 0755 "$BUILD_DIR/target/$TARGET/release/claw" "$BUILD_DIR/claw"
mv -f "$BUILD_DIR/claw" "$CLAW_BIN"
provenance > "$BUILD_DIR/claw.provenance"
mv -f "$BUILD_DIR/claw.provenance" "$CLAW_BIN.provenance"
printf '%s\n' "$CLAW_BIN"
