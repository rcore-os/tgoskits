#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(cd "$script_dir/../../.." && pwd)"
base_rootfs="$workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
output_rootfs="$workspace/tmp/axbuild/rootfs/rootfs-x86_64-deepseek.img"
api_key=""
proxy_url="${DEEPSEEK_ONLINE_PROXY:-}"
env_file=""
ca_cert="${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Prepare a local rootfs for the StarryOS DeepSeek TUI example.

Options:
  --api-key KEY          DeepSeek API key to inject (sets DEEPSEEK_API_KEY env in guest)
  --proxy URL            Write /root/.deepseek/starry-online-env with HTTP(S)/ALL proxy exports
                         (default: DEEPSEEK_ONLINE_PROXY when set)
  --env-file PATH        Inject an existing shell env file as /root/.deepseek/starry-online-env
  --base-rootfs PATH     Base rootfs image to copy before injection
                         (default: tmp/axbuild/rootfs/rootfs-x86_64-alpine.img)
  --output-rootfs PATH   Output rootfs image for the example
                         (default: tmp/axbuild/rootfs/rootfs-x86_64-deepseek.img)
  --ca-cert PATH         CA bundle to inject as /etc/ssl/certs/ca-certificates.crt
                         (default: SSL_CERT_FILE or /etc/ssl/certs/ca-certificates.crt)
  -h, --help             Show this help

Example (offline smoke test):
  apps/starry/deepseek-tui/prepare_deepseek_rootfs.sh

Example (online prime test):
  apps/starry/deepseek-tui/prepare_deepseek_rootfs.sh \\
    --output-rootfs tmp/axbuild/rootfs/rootfs-x86_64-deepseek-online.img \\
    --api-key sk-your-key-here \\
    --proxy http://10.0.2.2:7890
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

shell_quote() {
    local value="$1"
    printf "'%s'" "${value//\'/\'\\\'\'}"
}

workspace_path() {
    local path="$1"
    if [[ "$path" = /* ]]; then
        printf '%s\n' "$path"
    else
        printf '%s/%s\n' "$workspace" "$path"
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-key)
            api_key="$2"
            shift 2
            ;;
        --proxy)
            proxy_url="$2"
            shift 2
            ;;
        --env-file)
            env_file="$2"
            shift 2
            ;;
        --base-rootfs)
            base_rootfs="$2"
            shift 2
            ;;
        --output-rootfs)
            output_rootfs="$2"
            shift 2
            ;;
        --ca-cert)
            ca_cert="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

base_rootfs="$(workspace_path "$base_rootfs")"
output_rootfs="$(workspace_path "$output_rootfs")"
ca_cert="$(workspace_path "$ca_cert")"
if [[ -n "$env_file" ]]; then
    env_file="$(workspace_path "$env_file")"
fi

need_cmd cp
need_cmd debugfs
need_cmd install
need_cmd mktemp
need_cmd stat

if [[ ! -f "$base_rootfs" ]]; then
    if [[ "$base_rootfs" == "$workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img" ]]; then
        echo "Base rootfs not found; preparing the default x86_64 Alpine rootfs..."
        (cd "$workspace" && cargo xtask starry rootfs --arch x86_64)
    fi
fi

if [[ ! -f "$base_rootfs" ]]; then
    echo "error: base rootfs not found: $base_rootfs" >&2
    exit 1
fi

"$script_dir/prepare_deepseek_assets.sh"

deepseek_bin="$workspace/target/deepseek/assets/deepseek"
deepseek_tui_bin="$workspace/target/deepseek/assets/deepseek-tui"
if [[ ! -x "$deepseek_bin" || ! -x "$deepseek_tui_bin" ]]; then
    echo "error: DeepSeek TUI assets are missing after prepare_deepseek_assets.sh" >&2
    exit 1
fi

mkdir -p "$(dirname "$output_rootfs")"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/starry-deepseek-rootfs.XXXXXX")"
cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

overlay="$tmp_dir/overlay"
mkdir -p "$overlay/usr/local/bin" "$overlay/usr/lib" "$overlay/root/.deepseek"

# Inject shared libraries needed by the dynamically-linked binaries
lib_dir="$workspace/target/deepseek/assets/lib"
if [ -d "$lib_dir" ]; then
    for lib in "$lib_dir"/*.so*; do
        [ -f "$lib" ] && install -m 0644 "$lib" "$overlay/usr/lib/"
    done
fi
install -m 0755 "$deepseek_bin" "$overlay/usr/local/bin/deepseek"
install -m 0755 "$deepseek_tui_bin" "$overlay/usr/local/bin/deepseek-tui"

# Write env file with DEEPSEEK_API_KEY, optional proxy settings, or injected env file
if [[ -n "$env_file" ]]; then
    install -m 0644 "$env_file" "$overlay/root/.deepseek/starry-online-env"
else
    {
        if [[ -n "$api_key" ]]; then
            quoted_key="$(shell_quote "$api_key")"
            echo "export DEEPSEEK_API_KEY=$quoted_key"
        fi
        if [[ -n "$proxy_url" ]]; then
            quoted_proxy="$(shell_quote "$proxy_url")"
            cat <<ENVEOF
export HTTP_PROXY=$quoted_proxy
export HTTPS_PROXY=$quoted_proxy
export ALL_PROXY=$quoted_proxy
export http_proxy=$quoted_proxy
export https_proxy=$quoted_proxy
export all_proxy=$quoted_proxy
export NO_PROXY='localhost,127.0.0.1,::1'
export no_proxy='localhost,127.0.0.1,::1'
ENVEOF
        fi
    } > "$overlay/root/.deepseek/starry-online-env"
fi

if [[ -f "$ca_cert" ]]; then
    mkdir -p "$overlay/etc/ssl/certs"
    install -m 0644 "$ca_cert" "$overlay/etc/ssl/certs/ca-certificates.crt"
else
    echo "warning: CA bundle not found at $ca_cert; relying on the base rootfs CA bundle" >&2
fi

tmp_rootfs="$tmp_dir/rootfs.img"
cp --reflink=auto "$base_rootfs" "$tmp_rootfs" 2>/dev/null || cp "$base_rootfs" "$tmp_rootfs"

debugfs_script="$tmp_dir/inject.debugfs"
{
    find "$overlay" -type d | sort | while IFS= read -r dir; do
        rel="${dir#"$overlay"}"
        [[ -z "$rel" ]] && continue
        printf 'mkdir %s\n' "$rel"
    done
    find "$overlay" -type f | sort | while IFS= read -r file; do
        rel="${file#"$overlay"}"
        mode="$(stat -c '%a' "$file")"
        printf 'rm %s\n' "$rel"
        printf 'write %s %s\n' "$file" "$rel"
        printf 'sif %s mode 0100%s\n' "$rel" "$mode"
    done
    printf 'quit\n'
} > "$debugfs_script"

debugfs_log="$tmp_dir/debugfs.log"
if ! debugfs -w -f "$debugfs_script" "$tmp_rootfs" >"$debugfs_log" 2>&1; then
    cat "$debugfs_log" >&2
    exit 1
fi
mv "$tmp_rootfs" "$output_rootfs"

display_rootfs="$output_rootfs"
case "$display_rootfs" in
    "$workspace"/*)
        display_rootfs="${display_rootfs#"$workspace"/}"
        ;;
esac

echo "DeepSeek TUI example rootfs ready:"
echo "  $output_rootfs"
echo
echo "Run the offline smoke test with:"
echo "  cargo xtask starry qemu --arch x86_64 \\"
echo "    --qemu-config apps/starry/deepseek-tui/qemu-x86_64.toml \\"
echo "    --rootfs $display_rootfs"
if [[ -n "$api_key" ]]; then
    echo
    echo "Run the online prime demo with:"
    echo "  cargo xtask starry qemu --arch x86_64 \\"
    echo "    --qemu-config apps/starry/deepseek-tui/qemu-x86_64-deepseek-prime-test.toml \\"
    echo "    --rootfs $display_rootfs"
fi
