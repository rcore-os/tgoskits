#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
out_dir="$workspace/target/starry-macos-selfbuild"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

if [[ ! -f "$workspace/Cargo.toml" ]]; then
    echo "error: STARRY_WORKSPACE does not look like TGOSKits: $workspace" >&2
    exit 1
fi

shell_quote() {
    local value="$1"
    local i char
    printf "'"
    for ((i = 0; i < ${#value}; i++)); do
        char="${value:i:1}"
        if [[ "$char" == "'" ]]; then
            printf '%s' "'\\''"
        else
            printf '%s' "$char"
        fi
    done
    printf "'"
}

git_value() {
    local fallback="$1"
    shift
    git -C "$workspace" "$@" 2>/dev/null || printf '%s\n' "$fallback"
}

actual_commit="$(git_value unknown rev-parse HEAD)"
if [[ -n "${TGOSKITS_COMMIT:-}" && "$actual_commit" != "unknown" && "$TGOSKITS_COMMIT" != "$actual_commit" ]]; then
    echo "error: TGOSKITS_COMMIT=$TGOSKITS_COMMIT does not match workspace HEAD $actual_commit" >&2
    exit 1
fi

source_commit="${TGOSKITS_COMMIT:-$actual_commit}"
source_ref="${TGOSKITS_REF:-$(git_value detached symbolic-ref --quiet --short HEAD)}"
dirty="unknown"
if git -C "$workspace" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [[ -n "$(git -C "$workspace" status --porcelain --untracked-files=all)" ]]; then
        dirty="true"
    else
        dirty="false"
    fi
fi

mkdir -p "$out_dir" "$overlay_dir/opt"

meta_file="$out_dir/tgoskits-src.meta"
cat >"$meta_file" <<EOF
commit=$source_commit
ref=$source_ref
dirty=$dirty
generated_by=apps/starry/macos-selfbuild/prebuild.sh
EOF

meta_in_tar="$out_dir/.tgoskits-source-meta"
cp "$meta_file" "$meta_in_tar"

src_tar="$out_dir/tgoskits-src.tar"
tar -C "$workspace" \
    --exclude .git \
    --exclude target \
    --exclude tmp \
    --exclude .cache \
    --exclude .idea \
    --exclude .vscode \
    -cf "$src_tar" .
tar -C "$out_dir" -rf "$src_tar" .tgoskits-source-meta

guest_runner="$out_dir/starry-macos-run.sh"
{
cat <<'EOF'
#!/bin/sh
set -eu
export JOBS="\${JOBS:-8}"
export SMP="\${SMP:-8}"
export SOURCE_TMPFS="\${SOURCE_TMPFS:-1}"
export PROFILE="\${PROFILE:-release}"
export BUILD_TARGET="\${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
export BUILD_PACKAGE="\${BUILD_PACKAGE:-starryos}"
export BUILD_BIN="\${BUILD_BIN:-starryos}"
export BUILD_STD="\${BUILD_STD:-core,alloc,compiler_builtins}"
export FEATURES="\${FEATURES:-qemu,gic-v3,cntv-timer,smp}"
export NO_DEFAULT_FEATURES="\${NO_DEFAULT_FEATURES:-0}"
export CARGO_SUBCOMMAND="\${CARGO_SUBCOMMAND:-build}"
export SOURCE_DIR="\${SOURCE_DIR:-/opt/tgoskits}"
export WORK_DIR="\${WORK_DIR:-/tmp/starryos-selfbuild-src}"
export CARGO_TARGET_DIR="\${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
export CARGO_PROFILE_RELEASE_LTO="\${CARGO_PROFILE_RELEASE_LTO:-false}"
export CARGO_PROFILE_RELEASE_OPT_LEVEL="\${CARGO_PROFILE_RELEASE_OPT_LEVEL:-0}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="\${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-256}"
export CARGO_PROFILE_RELEASE_DEBUG="\${CARGO_PROFILE_RELEASE_DEBUG:-0}"
EOF
printf 'if [ -z "${TGOSKITS_COMMIT:-}" ]; then export TGOSKITS_COMMIT=%s; fi\n' "$(shell_quote "$source_commit")"
printf 'if [ -z "${TGOSKITS_REF:-}" ]; then export TGOSKITS_REF=%s; fi\n' "$(shell_quote "$source_ref")"
cat <<'EOF'
exec /bin/sh /opt/starry-macos-selfbuild.sh
EOF
} >"$guest_runner"
chmod 0755 "$guest_runner"

install -m 0755 "$app_dir/guest-selfbuild.sh" "$overlay_dir/opt/starry-macos-selfbuild.sh"
install -m 0755 "$guest_runner" "$overlay_dir/opt/starry-macos-run.sh"
install -m 0644 "$src_tar" "$overlay_dir/opt/tgoskits-src.tar"
install -m 0644 "$meta_file" "$overlay_dir/opt/tgoskits-src.meta"

echo "macos-selfbuild overlay ready in $overlay_dir"
echo "source_commit=$source_commit"
echo "source_ref=$source_ref"
echo "source_dirty=$dirty"
