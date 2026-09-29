#!/usr/bin/env bash
# Assemble the virtual UVC/RKNN + FT232 CI package from a prepared source tree
# and the local build output. This script never downloads sources, copies from a
# Git prebuilt directory, or deploys to a board.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: prepare-vision-usb-package.sh [OPTIONS]

Assemble the aka-rk3588 vision + FT232 deployment package consumed by the
virtual CI checks. The source tree is prepared by prepare-vision-usb-source.sh;
the build output supplies build/tennis and the AArch64 runtime libraries.

Required input:
  --build-dir DIR         build output directory (must contain build/tennis or
                          tennis; its lib/ directory is used by default)
  --source-dir DIR        prepared source tree (default:
                          <workspace>/target/aka-rk3588-vision-usb/source)

Optional:
  --binary PATH           explicit build/tennis path
  --runtime-lib-dir DIR   directory with libuvc.so.0, libusb-1.0.so.0,
                          libturbojpeg.so.0, libjpeg.so.8 and libudev.so.1
                          (default: <build-dir>/lib)
  --source-meta FILE      source metadata written by prepare-vision-usb-source.sh
                          (default: <source-dir>/../SOURCE)
  --out-dir DIR           output directory
                          (default: <workspace>/target/aka-rk3588-vision-usb)
  -h, --help              show this help

Output:
  <out-dir>/aka-rk3588-vision-usb.tar.gz
  <out-dir>/SOURCE

Example:
  ./prepare-vision-usb-package.sh \
    --source-dir target/aka-rk3588-vision-usb/source \
    --build-dir /path/to/aka-rk3588-virtual-work/build-output
EOF
}

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(git -C "$app_dir" rev-parse --show-toplevel 2>/dev/null || true)"
default_dir="${workspace:-$app_dir}/target/aka-rk3588-vision-usb"

source_dir="$default_dir/source"
source_meta=""
out_dir="$default_dir"
build_dir=""
binary=""
runtime_lib_dir=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --source-dir)
            source_dir="${2:-}"
            [ -n "$source_dir" ] || { echo "missing value for --source-dir" >&2; exit 2; }
            shift 2
            ;;
        --build-dir)
            build_dir="${2:-}"
            [ -n "$build_dir" ] || { echo "missing value for --build-dir" >&2; exit 2; }
            shift 2
            ;;
        --binary)
            binary="${2:-}"
            [ -n "$binary" ] || { echo "missing value for --binary" >&2; exit 2; }
            shift 2
            ;;
        --runtime-lib-dir)
            runtime_lib_dir="${2:-}"
            [ -n "$runtime_lib_dir" ] ||
                { echo "missing value for --runtime-lib-dir" >&2; exit 2; }
            shift 2
            ;;
        --source-meta)
            source_meta="${2:-}"
            [ -n "$source_meta" ] || { echo "missing value for --source-meta" >&2; exit 2; }
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

case "$out_dir" in
    ""|"/")
        echo "refusing unsafe output directory: $out_dir" >&2
        exit 2
        ;;
esac

fail() {
    echo "aka-rk3588 vision-USB package: $1" >&2
    exit 1
}

for tool in cp chmod dirname find mkdir mktemp realpath rm sha256sum sort tar xargs awk readlink; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing required command: $tool"
done

[ -d "$source_dir" ] || fail "prepared source directory does not exist: $source_dir"
out_dir_real="$(realpath -m "$out_dir")"
source_dir_real="$(realpath -m "$source_dir")"
case "$out_dir_real" in
    "$source_dir_real"|"$source_dir_real"/*)
        fail "refusing to write the package output inside the source directory: $out_dir"
        ;;
esac
for required in run_vision_usb_ci_once.sh models/tennis.rknn \
    3rd/rknpu2/Linux/aarch64/librknnrt.so; do
    [ -e "$source_dir/$required" ] ||
        fail "source directory is missing the required file: $required"
done

if [ -z "$binary" ]; then
    [ -n "$build_dir" ] || fail "provide --build-dir or --binary"
    if [ -f "$build_dir/build/tennis" ]; then
        binary="$build_dir/build/tennis"
    elif [ -f "$build_dir/tennis" ]; then
        binary="$build_dir/tennis"
    else
        fail "build output does not contain build/tennis or tennis: $build_dir"
    fi
fi
[ -x "$binary" ] || fail "built tennis binary is missing or not executable: $binary"

if [ -z "$runtime_lib_dir" ]; then
    [ -n "$build_dir" ] || fail "--runtime-lib-dir is required when --binary is used"
    runtime_lib_dir="$build_dir/lib"
fi
[ -d "$runtime_lib_dir" ] || fail "runtime library directory does not exist: $runtime_lib_dir"

if [ -z "$source_meta" ]; then
    source_meta="$(dirname "$source_dir")/SOURCE"
fi

meta_value() {
    local key="$1"
    if [ -f "$source_meta" ]; then
        awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, ""); print; exit }' \
            "$source_meta"
    fi
}

repository="$(meta_value repository)"
commit="$(meta_value commit)"
expected_tree_sha256="$(meta_value source_tree_sha256)"
repository="${repository:-unknown}"
commit="${commit:-unknown}"

tree_sha256="$(
    cd "$source_dir"
    find . -type f -print0 | LC_ALL=C sort -z |
        xargs -0 sha256sum | sha256sum | awk '{ print $1 }'
)"
if [ -n "$expected_tree_sha256" ] && [ "$tree_sha256" != "$expected_tree_sha256" ]; then
    fail "source tree SHA256 mismatch: got $tree_sha256, expected $expected_tree_sha256"
fi

runtime_lib_required=(
    libjpeg.so.8
    libturbojpeg.so.0
    libudev.so.1
    libusb-1.0.so.0
    libuvc.so.0
)

for name in "${runtime_lib_required[@]}"; do
    path="$runtime_lib_dir/$name"
    [ -e "$path" ] || fail "required runtime library is missing: $path"
done

runtime_libs_manifest() {
    local lib_dir="$1"
    local name path digest
    while IFS= read -r name; do
        path="$lib_dir/$name"
        if [ -L "$path" ]; then
            printf 'symlink:%s  %s\n' "$(readlink -- "$path")" "$name"
        elif [ -f "$path" ]; then
            digest="$(sha256sum -- "$path" | awk '{ print $1 }')"
            printf 'sha256:%s  %s\n' "$digest" "$name"
        else
            fail "unsupported entry in runtime library directory: $name"
        fi
    done < <(find "$lib_dir" -mindepth 1 -maxdepth 1 ! -name 'librknnrt.so' -printf '%f\n' |
        LC_ALL=C sort)
}

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
package_dir="$work_dir/aka-rk3588"
package_archive="$out_dir/aka-rk3588-vision-usb.tar.gz"

mkdir -p "$out_dir" "$package_dir/build" "$package_dir/lib"
cp "$binary" "$package_dir/build/tennis"
cp -a "$runtime_lib_dir/." "$package_dir/lib/"
cp "$source_dir/3rd/rknpu2/Linux/aarch64/librknnrt.so" \
    "$package_dir/lib/librknnrt.so"
cp -a "$source_dir/models" "$package_dir/"
cp "$source_dir/run_vision_usb_ci_once.sh" "$package_dir/run_vision_usb_ci_once.sh"
chmod +x "$package_dir/build/tennis" "$package_dir"/*.sh

binary_sha256="$(sha256sum "$package_dir/build/tennis" | awk '{ print $1 }')"
launcher_sha256="$(sha256sum "$package_dir/run_vision_usb_ci_once.sh" | awk '{ print $1 }')"
model_sha256="$(sha256sum "$package_dir/models/tennis.rknn" | awk '{ print $1 }')"
librknnrt_sha256="$(sha256sum "$package_dir/lib/librknnrt.so" | awk '{ print $1 }')"
runtime_libs_sha256="$(runtime_libs_manifest "$package_dir/lib" | sha256sum | awk '{ print $1 }')"

cat >"$package_dir/SOURCE" <<EOF
repository=$repository
commit=$commit
source_tree_sha256=$tree_sha256
binary_sha256=$binary_sha256
launcher_sha256=$launcher_sha256
model_sha256=$model_sha256
librknnrt_sha256=$librknnrt_sha256
runtime_libs_sha256=$runtime_libs_sha256
EOF

tar -czf "$package_archive" -C "$work_dir" aka-rk3588
cp "$package_dir/SOURCE" "$out_dir/SOURCE"

echo "aka-rk3588 vision-USB package: $package_archive"
echo "  source directory: $source_dir"
echo "  build binary: $binary"
echo "  runtime libraries: $runtime_lib_dir"
echo "  metadata: $out_dir/SOURCE"
