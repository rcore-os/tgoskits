#!/usr/bin/env bash
set -euo pipefail

workspace="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
app_dir="${STARRY_APP_DIR:-$workspace/apps/starry/deepseek-tui}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
asset_dir="$workspace/target/deepseek/assets"
api_key="${DEEPSEEK_API_KEY:-}"
proxy_url="${DEEPSEEK_ONLINE_PROXY:-}"
env_file="${DEEPSEEK_ONLINE_ENV_FILE:-}"
ca_cert="${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}"

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

populate_overlay() {
    if [[ -z "$overlay_dir" ]]; then
        echo "error: STARRY_OVERLAY_DIR is required" >&2
        exit 1
    fi

    "$app_dir/prepare_deepseek_assets.sh"

    local deepseek_bin="$asset_dir/deepseek"
    local deepseek_tui_bin="$asset_dir/deepseek-tui"
    if [[ ! -x "$deepseek_bin" || ! -x "$deepseek_tui_bin" ]]; then
        echo "error: DeepSeek TUI assets are missing after prepare_deepseek_assets.sh" >&2
        exit 1
    fi

    install -Dm0755 "$deepseek_bin" "$overlay_dir/usr/local/bin/deepseek"
    install -Dm0755 "$deepseek_tui_bin" "$overlay_dir/usr/local/bin/deepseek-tui"

    local lib_dir="$asset_dir/lib"
    if [[ -d "$lib_dir" ]]; then
        local lib
        for lib in "$lib_dir"/*.so*; do
            [[ -f "$lib" ]] && install -Dm0644 "$lib" "$overlay_dir/usr/lib/$(basename "$lib")"
        done
    fi

    mkdir -p "$overlay_dir/root/.deepseek"
    if [[ -n "$env_file" ]]; then
        env_file="$(workspace_path "$env_file")"
        if [[ ! -f "$env_file" ]]; then
            echo "error: env file not found: $env_file" >&2
            exit 1
        fi
        install -Dm0644 "$env_file" "$overlay_dir/root/.deepseek/starry-online-env"
    else
        {
            if [[ -n "$api_key" ]]; then
                local quoted_key
                quoted_key="$(shell_quote "$api_key")"
                echo "export DEEPSEEK_API_KEY=$quoted_key"
            fi
            if [[ -n "$proxy_url" ]]; then
                local quoted_proxy
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
        } > "$overlay_dir/root/.deepseek/starry-online-env"
        chmod 0644 "$overlay_dir/root/.deepseek/starry-online-env"
    fi

    ca_cert="$(workspace_path "$ca_cert")"
    if [[ -f "$ca_cert" ]]; then
        install -Dm0644 "$ca_cert" "$overlay_dir/etc/ssl/certs/ca-certificates.crt"
    else
        echo "warning: CA bundle not found at $ca_cert; relying on the base rootfs CA bundle" >&2
    fi
}

populate_overlay
