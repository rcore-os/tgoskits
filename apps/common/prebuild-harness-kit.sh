#!/usr/bin/env bash
set -euo pipefail

HARNESS_KIT_REPO="https://github.com/cg24-THU/tgoskit-harness_kit.git"
HARNESS_KIT_COMMIT="762c22725024a065e85b26e0b01121eccea651c0"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-}"
if [[ -z "$workspace" ]]; then
    workspace="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

override_checkout="${TGOSKIT_HARNESS_KIT_DIR:-}"
checkout="${override_checkout:-$workspace/target/tgoskit-harness-kit/$HARNESS_KIT_COMMIT}"
tmp_checkout=""
lock_file=""
lock_token=""
lock_owned=0

release_checkout_lock() {
    if [[ "$lock_owned" -eq 1 && -e "$lock_file" && "$lock_token" -ef "$lock_file" ]]; then
        rm -f "$lock_file"
    fi
    lock_owned=0
    if [[ -n "$lock_token" ]]; then
        rm -f "$lock_token"
        lock_token=""
    fi
}

cleanup() {
    if [[ -n "$tmp_checkout" ]]; then
        rm -rf "$tmp_checkout"
    fi
    release_checkout_lock
}
trap cleanup EXIT

acquire_checkout_lock() {
    local parent
    local holder
    local attempts=0

    parent="$(dirname "$checkout")"
    mkdir -p "$parent"
    lock_file="$checkout.lock"
    lock_token="$(mktemp "$parent/.tgoskit-harness-kit-lock.XXXXXX")"
    printf '%s\n' "$$" >"$lock_token"

    while ! ln "$lock_token" "$lock_file" 2>/dev/null; do
        holder="$(head -n 1 "$lock_file" 2>/dev/null || true)"
        if [[ ! "$holder" =~ ^[0-9]+$ ]] || ! kill -0 "$holder" 2>/dev/null; then
            rm -f "$lock_file"
            continue
        fi
        attempts=$((attempts + 1))
        if [[ "$attempts" -ge 3000 ]]; then
            echo "error: timed out waiting for harness kit checkout lock $lock_file" >&2
            exit 1
        fi
        sleep 0.1
    done
    lock_owned=1
}

is_git_checkout() {
    git -C "$1" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

require_file() {
    local path="$1"
    local file="$2"
    if [[ ! -f "$path/$file" ]]; then
        echo "error: missing $file in $path" >&2
        exit 1
    fi
}

validate_checkout() {
    local path="$1"
    local label="$2"

    require_file "$path" "tools/qperf/Cargo.toml"
    require_file "$path" "tools/qperf/analyzer/Cargo.toml"
    require_file "$path" "tools/starry-syscall-harness/harness.py"

    if ! is_git_checkout "$path"; then
        echo "error: $label is not a git checkout; cannot verify pinned harness kit commit $HARNESS_KIT_COMMIT" >&2
        exit 1
    fi

    actual="$(git -C "$path" rev-parse HEAD)"
    if [[ "$actual" != "$HARNESS_KIT_COMMIT" ]]; then
        echo "error: $label is at commit $actual, expected $HARNESS_KIT_COMMIT" >&2
        if [[ -n "$override_checkout" ]]; then
            echo "error: TGOSKIT_HARNESS_KIT_DIR is read-only and will not be fetched, reset, or replaced" >&2
        fi
        exit 1
    fi
}

ensure_checkout() {
    if [[ -n "$override_checkout" ]]; then
        validate_checkout "$checkout" "TGOSKIT_HARNESS_KIT_DIR=$checkout"
        return
    fi

    acquire_checkout_lock

    # Another provider may have completed the checkout while this process was
    # waiting. Recheck under the lock before fetching or repairing anything.
    if is_git_checkout "$checkout" && [[ "$(git -C "$checkout" rev-parse HEAD)" == "$HARNESS_KIT_COMMIT" ]]; then
        validate_checkout "$checkout" "$checkout"
        release_checkout_lock
        return
    fi

    if ! is_git_checkout "$checkout"; then
        tmp_checkout="$(mktemp -d "$(dirname "$checkout")/.tgoskit-harness-kit.XXXXXX")"
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
        tmp_checkout=""
    else
        actual="$(git -C "$checkout" rev-parse HEAD)"
        if [[ "$actual" != "$HARNESS_KIT_COMMIT" ]]; then
            git -C "$checkout" fetch --depth 1 origin "$HARNESS_KIT_COMMIT"
            git -C "$checkout" checkout --detach "$HARNESS_KIT_COMMIT" >/dev/null
            git -C "$checkout" reset --hard "$HARNESS_KIT_COMMIT" >/dev/null
        fi
    fi

    validate_checkout "$checkout" "$checkout"
    release_checkout_lock
}

ensure_checkout

if [[ $# -gt 0 ]]; then
    cd "$checkout"
    exec "$@"
fi

printf '%s\n' "$checkout"
