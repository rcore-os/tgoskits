#!/usr/bin/env bash
# Resolve and prepare the aka-rk3588 `virtual` source tree consumed by the
# virtual UVC/RKNN + FT232 CI package.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: prepare-vision-usb-source.sh [OPTIONS]

Prepare the aka-rk3588 virtual source tree for the vision + FT232 CI package.
Without --checkout the script resolves the configured branch on the remote
repository to a full commit SHA and downloads that archive. With --checkout it
archives the requested ref from a local Git checkout. A source checkout is
never modified.

Options:
  --checkout DIR     local aka-rk3588 Git checkout to read
  --repository URL   source repository
                     (default: https://github.com/bullhh/aka-rk3588)
  --ref REF          local ref or commit when --checkout is used; remote branch
                     name otherwise (default: HEAD for --checkout, virtual for
                     a remote repository)
  --out-dir DIR      output directory
                     (default: <workspace>/target/aka-rk3588-vision-usb)
  -h, --help         show this help

Output:
  <out-dir>/source/  extracted source tree
  <out-dir>/SOURCE   repository, ref, full commit, archive hash, tree hash

Example:
  ./prepare-vision-usb-source.sh --checkout /path/to/aka-rk3588-virtual-work
EOF
}

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(git -C "$app_dir" rev-parse --show-toplevel 2>/dev/null || true)"

repository="https://github.com/bullhh/aka-rk3588"
ref=""
checkout=""
out_dir=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --checkout)
            checkout="${2:-}"
            [ -n "$checkout" ] || { echo "missing value for --checkout" >&2; exit 2; }
            shift 2
            ;;
        --repository)
            repository="${2:-}"
            [ -n "$repository" ] || { echo "missing value for --repository" >&2; exit 2; }
            shift 2
            ;;
        --ref)
            ref="${2:-}"
            [ -n "$ref" ] || { echo "missing value for --ref" >&2; exit 2; }
            shift 2
            ;;
        --out-dir)
            out_dir="${2:-}"
            [ -n "$out_dir" ] || { echo "missing value for --out-dir" >&2; exit 2; }
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$out_dir" ]; then
    out_dir="${workspace:-$app_dir}/target/aka-rk3588-vision-usb"
fi
case "$out_dir" in
    ""|"/")
        echo "refusing unsafe output directory: $out_dir" >&2
        exit 2
        ;;
esac

source_dir="$out_dir/source"
cache_dir="$out_dir/cache"

fail() {
    echo "aka-rk3588 vision-USB source: $1" >&2
    exit 1
}

for tool in git gzip realpath sha256sum tar find sort xargs awk; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing required command: $tool"
done

if [ -n "$checkout" ]; then
    [ -d "$checkout" ] || fail "checkout directory does not exist: $checkout"
    [ -n "$ref" ] || ref="HEAD"
    checkout_real="$(realpath -m "$checkout")"
    source_dir_real="$(realpath -m "$source_dir")"
    case "$source_dir_real" in
        "$checkout_real"|"$checkout_real"/*)
            fail "refusing to write source output inside the checkout: $source_dir"
            ;;
    esac
    commit="$(git -C "$checkout" rev-parse --verify --quiet "${ref}^{commit}")" ||
        fail "cannot resolve ${ref} in $checkout"
    repository="$(git -C "$checkout" remote get-url origin 2>/dev/null || echo "$repository")"
else
    [ -n "$ref" ] || ref="virtual"
    repository="${repository%.git}"
    commit="$(git ls-remote --exit-code "$repository" "refs/heads/$ref" 2>/dev/null |
        awk 'NR == 1 { print $1 }')" ||
        fail "cannot resolve remote branch $ref in $repository"
fi

if [ "${#commit}" -ne 40 ]; then
    fail "resolved commit is not a full 40-character SHA: $commit"
fi
case "$commit" in
    *[!0-9a-f]*)
        fail "resolved commit is not lowercase hexadecimal: $commit"
        ;;
esac

if [ -n "$checkout" ]; then
    # Keep local and remote cache entries separate: both archives now carry one
    # top-level prefix for --strip-components=1, but an older local archive may
    # have been produced without that prefix.
    archive="$cache_dir/aka-rk3588-vision-usb-local-$commit.tar.gz"
else
    archive="$cache_dir/aka-rk3588-vision-usb-remote-$commit.tar.gz"
fi
mkdir -p "$cache_dir" "$out_dir"

if [ ! -f "$archive" ]; then
    archive_tmp="$archive.tmp"
    rm -f "$archive_tmp"
    if [ -n "$checkout" ]; then
        git -C "$checkout" archive --format=tar --prefix=aka-rk3588/ "$commit" |
            gzip -n -9 >"$archive_tmp"
    else
        command -v curl >/dev/null 2>&1 || fail "missing required command: curl"
        curl --fail --location --retry 3 --output "$archive_tmp" \
            "$repository/archive/$commit.tar.gz"
    fi
    mv "$archive_tmp" "$archive"
fi

archive_sha256="$(sha256sum "$archive" | awk '{ print $1 }')"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

tar -xzf "$archive" --strip-components=1 -C "$stage"
for required in CMakeLists.txt build_rk3588.sh run_vision_usb_ci_once.sh \
    models/tennis.rknn 3rd/rknpu2/Linux/aarch64/librknnrt.so; do
    [ -e "$stage/$required" ] ||
        fail "commit $commit does not contain the required file: $required"
done

rm -rf "$source_dir"
mkdir -p "$source_dir"
cp -a "$stage/." "$source_dir/"

tree_sha256="$(
    cd "$source_dir"
    find . -type f -print0 | LC_ALL=C sort -z |
        xargs -0 sha256sum | sha256sum | awk '{ print $1 }'
)"

cat >"$out_dir/SOURCE" <<EOF
repository=$repository
ref=$ref
commit=$commit
source_archive_sha256=$archive_sha256
source_tree_sha256=$tree_sha256
EOF

echo "aka-rk3588 vision-USB source: $source_dir"
echo "  repository: $repository"
echo "  ref: $ref"
echo "  commit: $commit"
echo "  metadata: $out_dir/SOURCE"
