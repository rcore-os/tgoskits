#!/usr/bin/env bash
set -euo pipefail

workspace="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
app_dir="${STARRY_APP_DIR:-$workspace/apps/starry/picoclaw-cli}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
asset_dir="${PICOCLAW_ASSET_DIR:-$workspace/target/picoclaw/assets}"

version="${PICOCLAW_VERSION:-v0.2.8}"
asset_name="${PICOCLAW_ASSET_NAME:-picoclaw_Linux_x86_64.tar.gz}"
asset_sha256="${PICOCLAW_ASSET_SHA256:-e35aea853711db829e0d1969d875f2efcca9cfeec92a43dedb84b46a56b890be}"
asset_url="${PICOCLAW_ASSET_URL:-https://github.com/sipeed/picoclaw/releases/download/${version}/${asset_name}}"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/picoclaw-assets.XXXXXX")"
config_json="${PICOCLAW_CONFIG_JSON:-}"
security_yml="${PICOCLAW_SECURITY_YML:-}"
env_file="${PICOCLAW_ONLINE_ENV_FILE:-}"
api_key="${PICOCLAW_API_KEY:-${OPENAI_API_KEY:-${ANTHROPIC_AUTH_TOKEN:-}}}"
proxy_url="${PICOCLAW_ONLINE_PROXY:-}"
ca_cert="${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}"

model_name="${PICOCLAW_MODEL_NAME:-starry-smoke}"
provider="${PICOCLAW_PROVIDER:-openai}"
model="${PICOCLAW_MODEL:-gpt-5.4}"
api_base="${PICOCLAW_API_BASE:-https://api.openai.com/v1}"

if [[ -z "${PICOCLAW_PROVIDER:-}" && -z "${PICOCLAW_API_KEY:-}" && -z "${OPENAI_API_KEY:-}" && -n "${ANTHROPIC_AUTH_TOKEN:-}" ]]; then
    model_name="${PICOCLAW_MODEL_NAME:-claude-sonnet-4.6}"
    provider="anthropic-messages"
    model="${PICOCLAW_MODEL:-claude-sonnet-4.6}"
    api_base="${PICOCLAW_API_BASE:-${ANTHROPIC_BASE_URL:-https://api.anthropic.com/v1}}"
fi

cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

ensure_host_packages() {
    local missing=()

    command -v curl >/dev/null 2>&1 || missing+=(curl)
    command -v tar >/dev/null 2>&1 || missing+=(tar)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
        missing+=(coreutils)
    fi

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "error: missing required host packages and apt-get is unavailable: ${missing[*]}" >&2
        exit 1
    fi

    if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
        echo "error: missing required host packages: ${missing[*]}" >&2
        echo "error: install them first with: sudo apt-get install -y --no-install-recommends ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "error: missing required command: sha256sum or shasum" >&2
        exit 1
    fi
}

shell_quote() {
    local value="$1"
    printf "'%s'" "${value//\'/\'\\\'\'}"
}

json_escape() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    printf '%s' "$value"
}

yaml_double_quote() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "$value"
}

workspace_path() {
    local path="$1"
    if [[ "$path" = /* ]]; then
        printf '%s\n' "$path"
    else
        printf '%s/%s\n' "$workspace" "$path"
    fi
}

prepare_picoclaw_assets() {
    ensure_host_packages
    mkdir -p "$asset_dir"

    if [[ -x "$asset_dir/picoclaw" && -x "$asset_dir/picoclaw-launcher" ]]; then
        echo "PicoClaw assets already exist in $asset_dir"
        return
    fi

    local tarball="$tmp_dir/$asset_name"
    echo "Downloading $asset_url"
    curl -L --fail --retry 3 --output "$tarball" "$asset_url"

    local actual_sha256
    actual_sha256="$(sha256_of "$tarball")"
    if [[ "$actual_sha256" != "$asset_sha256" ]]; then
        echo "SHA-256 mismatch for $asset_name" >&2
        echo "expected: $asset_sha256" >&2
        echo "actual:   $actual_sha256" >&2
        exit 1
    fi

    tar -xzf "$tarball" -C "$tmp_dir"

    if [[ ! -f "$tmp_dir/picoclaw" || ! -f "$tmp_dir/picoclaw-launcher" ]]; then
        echo "release asset does not contain expected PicoClaw binaries" >&2
        exit 1
    fi

    install -m 0755 "$tmp_dir/picoclaw" "$asset_dir/picoclaw"
    install -m 0755 "$tmp_dir/picoclaw-launcher" "$asset_dir/picoclaw-launcher"

    echo "PicoClaw assets ready in $asset_dir"
}

populate_overlay() {
    if [[ -z "$overlay_dir" ]]; then
        echo "error: STARRY_OVERLAY_DIR is required" >&2
        exit 1
    fi

    install -Dm0755 "$asset_dir/picoclaw" "$overlay_dir/usr/local/bin/picoclaw"
    install -Dm0755 "$asset_dir/picoclaw-launcher" "$overlay_dir/usr/local/bin/picoclaw-launcher"
    install -d "$overlay_dir/root/.picoclaw/workspace"

    if [[ -n "$config_json" ]]; then
        config_json="$(workspace_path "$config_json")"
        install -Dm0644 "$config_json" "$overlay_dir/root/.picoclaw/config.json"
    elif [[ -n "$api_key" ]]; then
        install -d "$overlay_dir/root/.picoclaw"
        cat >"$overlay_dir/root/.picoclaw/config.json" <<JSONEOF
{
  "version": 3,
  "agents": {
    "defaults": {
      "model_name": "$(json_escape "$model_name")",
      "workspace": "/root/.picoclaw/workspace",
      "restrict_to_workspace": true,
      "max_tool_iterations": 3
    }
  },
  "model_list": [
    {
      "model_name": "$(json_escape "$model_name")",
      "provider": "$(json_escape "$provider")",
      "model": "$(json_escape "$model")",
      "api_base": "$(json_escape "$api_base")",
      "enabled": true
    }
  ],
  "gateway": {
    "host": "127.0.0.1",
    "port": 18790,
    "hot_reload": false,
    "log_level": "warn"
  }
}
JSONEOF
        chmod 0644 "$overlay_dir/root/.picoclaw/config.json"
    fi

    if [[ -n "$security_yml" ]]; then
        security_yml="$(workspace_path "$security_yml")"
        install -Dm0600 "$security_yml" "$overlay_dir/root/.picoclaw/.security.yml"
    elif [[ -n "$api_key" ]]; then
        install -d "$overlay_dir/root/.picoclaw"
        {
            printf 'model_list:\n'
            printf '  %s:\n' "$(yaml_double_quote "$model_name")"
            printf '    api_keys:\n'
            printf '      - %s\n' "$(yaml_double_quote "$api_key")"
        } >"$overlay_dir/root/.picoclaw/.security.yml"
        chmod 0600 "$overlay_dir/root/.picoclaw/.security.yml"
    fi

    if [[ -n "$env_file" ]]; then
        env_file="$(workspace_path "$env_file")"
        install -Dm0600 "$env_file" "$overlay_dir/root/.picoclaw/starry-online-env"
    elif [[ -n "$proxy_url" ]]; then
        install -d "$overlay_dir/root/.picoclaw"
        {
            printf 'export http_proxy=%s\n' "$(shell_quote "$proxy_url")"
            printf 'export https_proxy=%s\n' "$(shell_quote "$proxy_url")"
            printf 'export all_proxy=%s\n' "$(shell_quote "$proxy_url")"
            printf 'export HTTP_PROXY=%s\n' "$(shell_quote "$proxy_url")"
            printf 'export HTTPS_PROXY=%s\n' "$(shell_quote "$proxy_url")"
            printf 'export ALL_PROXY=%s\n' "$(shell_quote "$proxy_url")"
        } >"$overlay_dir/root/.picoclaw/starry-online-env"
        chmod 0600 "$overlay_dir/root/.picoclaw/starry-online-env"
    fi

    ca_cert="$(workspace_path "$ca_cert")"
    if [[ -n "$ca_cert" && -f "$ca_cert" ]]; then
        install -Dm0644 "$ca_cert" "$overlay_dir/etc/ssl/certs/ca-certificates.crt"
    fi
}

main() {
    : "$app_dir"

    if [[ -z "$overlay_dir" ]]; then
        echo "error: STARRY_OVERLAY_DIR is required" >&2
        exit 1
    fi

    prepare_picoclaw_assets
    populate_overlay
}

main "$@"
