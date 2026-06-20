#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  eval "$(apps/starry/macos-selfbuild/extract_kernel.sh)"

  eval "$(apps/starry/macos-selfbuild/extract_kernel.sh \
    --rootfs-copy target/starry-macos-selfbuild/rootfs/rootfs-smp8-j8.img \
    --output-stem starryos-selfbuilt-smp8-j8)"

Extracts the guest-built StarryOS kernel artifacts from a self-build rootfs copy.
By default it selects the newest image under:

  target/starry-macos-selfbuild/rootfs/rootfs-*.img

It prints shell assignments to stdout:

  rootfs_copy=...
  kernel_elf=...
  kernel_bin=...

All status and error messages are written to stderr so stdout can be used with
eval.
USAGE
}

find_debugfs() {
    if [[ -n "${DEBUGFS:-}" ]]; then
        printf '%s\n' "$DEBUGFS"
    elif command -v debugfs >/dev/null 2>&1; then
        command -v debugfs
    elif [[ -x /opt/homebrew/opt/e2fsprogs/sbin/debugfs ]]; then
        printf '%s\n' /opt/homebrew/opt/e2fsprogs/sbin/debugfs
    else
        echo "debugfs not found; install e2fsprogs or set DEBUGFS=/path/to/debugfs" >&2
        exit 1
    fi
}

latest_rootfs_copy() {
    local rootfs_dir="$repo_root/target/starry-macos-selfbuild/rootfs"
    local -a candidates=()

    if [[ -d "$rootfs_dir" ]]; then
        shopt -s nullglob
        candidates=("$rootfs_dir"/rootfs-*.img)
        shopt -u nullglob
    fi
    if [[ "${#candidates[@]}" -eq 0 ]]; then
        echo "no self-build rootfs copy found under $rootfs_dir" >&2
        echo "run apps/starry/macos-selfbuild/run_selfbuild.sh first, or pass --rootfs-copy" >&2
        exit 1
    fi
    ls -t "${candidates[@]}" | head -1
}

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

emit_assignment() {
    local name="$1"
    local value="$2"
    printf '%s=' "$name"
    shell_quote "$value"
    printf '\n'
}

abs_existing_file() {
    local path="$1"
    local dir base

    if [[ ! -f "$path" ]]; then
        echo "file not found: $path" >&2
        exit 1
    fi
    dir="$(cd "$(dirname "$path")" && pwd)"
    base="$(basename "$path")"
    printf '%s/%s\n' "$dir" "$base"
}

abs_dir() {
    local path="$1"
    mkdir -p "$path"
    cd "$path" && pwd
}

dump_guest_file() {
    local debugfs="$1"
    local rootfs="$2"
    local guest_path="$3"
    local host_path="$4"
    local label="$5"
    local err_file bytes

    err_file="$(mktemp "${TMPDIR:-/tmp}/starry-extract.XXXXXX")"
    rm -f "$host_path"
    if ! "$debugfs" -R "dump $guest_path $host_path" "$rootfs" >/dev/null 2>"$err_file"; then
        cat "$err_file" >&2
        rm -f "$err_file"
        echo "failed to extract $guest_path from $rootfs" >&2
        exit 1
    fi
    rm -f "$err_file"

    if [[ ! -s "$host_path" ]]; then
        echo "extracted $label is empty: $host_path" >&2
        exit 1
    fi

    bytes="$(wc -c <"$host_path" | tr -d '[:space:]')"
    echo "extracted $label: $host_path ($bytes bytes)" >&2
}

rootfs_copy="${ROOTFS_COPY:-}"
output_dir="${OUTPUT_DIR:-$repo_root/target/starry-macos-selfbuild/extracted}"
output_stem="${OUTPUT_STEM:-}"
target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
guest_elf_path="${GUEST_ELF_PATH:-/opt/starryos-selfbuild-artifacts/starryos-${target}}"
guest_bin_path="${GUEST_BIN_PATH:-${guest_elf_path}.bin}"
extract_bin="${EXTRACT_BIN:-1}"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --rootfs-copy|--rootfs)
            rootfs_copy="$2"
            shift 2
            ;;
        --output-dir)
            output_dir="$2"
            shift 2
            ;;
        --output-stem|--name)
            output_stem="$2"
            shift 2
            ;;
        --target)
            target="$2"
            guest_elf_path="/opt/starryos-selfbuild-artifacts/starryos-${target}"
            guest_bin_path="${guest_elf_path}.bin"
            shift 2
            ;;
        --guest-elf-path)
            guest_elf_path="$2"
            shift 2
            ;;
        --guest-bin-path)
            guest_bin_path="$2"
            shift 2
            ;;
        --no-bin)
            extract_bin=0
            shift
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

if [[ -z "$rootfs_copy" ]]; then
    rootfs_copy="$(latest_rootfs_copy)"
fi
rootfs_copy="$(abs_existing_file "$rootfs_copy")"
output_dir="$(abs_dir "$output_dir")"

if [[ -z "$output_stem" ]]; then
    rootfs_base="$(basename "$rootfs_copy" .img)"
    rootfs_base="${rootfs_base#rootfs-}"
    output_stem="starryos-selfbuilt-${rootfs_base}"
fi

debugfs="$(find_debugfs)"
kernel_elf="$output_dir/$output_stem"
kernel_bin="$kernel_elf.bin"

dump_guest_file "$debugfs" "$rootfs_copy" "$guest_elf_path" "$kernel_elf" "kernel_elf"
if [[ "$extract_bin" = "1" ]]; then
    dump_guest_file "$debugfs" "$rootfs_copy" "$guest_bin_path" "$kernel_bin" "kernel_bin"
else
    kernel_bin=""
fi

emit_assignment rootfs_copy "$rootfs_copy"
emit_assignment kernel_elf "$kernel_elf"
if [[ -n "$kernel_bin" ]]; then
    emit_assignment kernel_bin "$kernel_bin"
fi
