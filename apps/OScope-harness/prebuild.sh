#!/usr/bin/env bash
set -euo pipefail

HARNESS_KIT_REPO="https://github.com/cg24-THU/tgoskit-harness_kit.git"
HARNESS_KIT_COMMIT="b4fdf12c8479353d80e3d23960e653819db2a20d"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-}"
if [[ -z "$workspace" ]]; then
    workspace="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

checkout="${TGOSKIT_HARNESS_KIT_DIR:-$workspace/target/tgoskit-harness-kit/$HARNESS_KIT_COMMIT}"
tmp_checkout="$checkout.tmp.$$"

cleanup() {
    rm -rf "$tmp_checkout"
}
trap cleanup EXIT

ensure_checkout() {
    if [[ ! -d "$checkout/.git" ]]; then
        mkdir -p "$(dirname "$checkout")"
        git init -q "$tmp_checkout"
        git -C "$tmp_checkout" remote add origin "$HARNESS_KIT_REPO"
        git -C "$tmp_checkout" fetch --depth 1 origin "$HARNESS_KIT_COMMIT"
        git -C "$tmp_checkout" checkout --detach FETCH_HEAD >/dev/null
        actual="$(git -C "$tmp_checkout" rev-parse HEAD)"
        if [[ "$actual" != "$HARNESS_KIT_COMMIT" ]]; then
            echo "error: fetched $actual, expected $HARNESS_KIT_COMMIT" >&2
            exit 1
        fi
        rm -rf "$checkout"
        mv "$tmp_checkout" "$checkout"
        return
    fi

    actual="$(git -C "$checkout" rev-parse HEAD)"
    if [[ "$actual" != "$HARNESS_KIT_COMMIT" ]]; then
        git -C "$checkout" fetch --depth 1 origin "$HARNESS_KIT_COMMIT"
        git -C "$checkout" checkout --detach "$HARNESS_KIT_COMMIT" >/dev/null
        git -C "$checkout" reset --hard "$HARNESS_KIT_COMMIT" >/dev/null
    fi
}

ensure_checkout

if [[ $# -gt 0 ]]; then
    cd "$checkout"
    exec "$@"
fi

printf '%s\n' "$checkout"
