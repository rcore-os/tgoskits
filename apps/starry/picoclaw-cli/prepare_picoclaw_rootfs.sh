#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
example_dir="${workspace}/apps/starry/picoclaw-cli"

default_rootfs="${workspace}/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
rootfs="$default_rootfs"
asset_dir="${workspace}/target/picoclaw/assets"
config_json=""
security_yml=""
env_file=""
api_key="${PICOCLAW_API_KEY:-${OPENAI_API_KEY:-${ANTHROPIC_AUTH_TOKEN:-}}}"
proxy_url="${PICOCLAW_ONLINE_PROXY:-}"
ca_cert="${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}"

model_name="${PICOCLAW_MODEL_NAME:-mimo-v25}"
provider="${PICOCLAW_PROVIDER:-openai}"
model="${PICOCLAW_MODEL:-mimo-v2.5}"
api_base="${PICOCLAW_API_BASE:-https://token-plan-cn.xiaomimimo.com/v1}"
disable_thinking="${PICOCLAW_DISABLE_THINKING:-auto}"

if [[ -z "${PICOCLAW_PROVIDER:-}" && -z "${PICOCLAW_API_KEY:-}" && -z "${OPENAI_API_KEY:-}" && -n "${ANTHROPIC_AUTH_TOKEN:-}" ]]; then
    model_name="${PICOCLAW_MODEL_NAME:-claude-sonnet-4.6}"
    provider="anthropic-messages"
    model="${PICOCLAW_MODEL:-claude-sonnet-4.6}"
    api_base="${PICOCLAW_API_BASE:-${ANTHROPIC_BASE_URL:-https://api.anthropic.com/v1}}"
    disable_thinking="${PICOCLAW_DISABLE_THINKING:-false}"
fi

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Build a StarryOS x86_64 rootfs with PicoClaw injected.

Options:
  --rootfs PATH           Rootfs image to update
  --config-json PATH      Inject a prepared /root/.picoclaw/config.json
  --security-yml PATH     Inject a prepared /root/.picoclaw/.security.yml
  --api-key KEY           Generate online config and secret security file
  --model-name NAME       Generated online config model name
  --provider NAME         Generated online config provider
  --model NAME            Generated online config model id
  --api-base URL          Generated online config API base
  --disable-thinking      Add extra_body.chat_template_kwargs.enable_thinking=false
  --enable-thinking       Do not add the Mimo thinking-disable extra_body
  --proxy URL             Inject http_proxy/https_proxy/all_proxy env file
  --env-file PATH         Inject a shell env file as starry-online-env
  --ca-cert PATH          Inject CA bundle as /etc/ssl/certs/ca-certificates.crt
  -h, --help              Show this help

Secrets and generated rootfs images stay under tmp/ or target/ and must not be
committed.

The generated online config uses PICOCLAW_API_KEY first, then OPENAI_API_KEY,
then ANTHROPIC_AUTH_TOKEN, or the explicit --api-key value. If
ANTHROPIC_AUTH_TOKEN is the selected source, the generated provider defaults to
Anthropic Messages and ANTHROPIC_BASE_URL is used as the API base when present.
EOF
}

while (($#)); do
    case "$1" in
        --rootfs)
            rootfs="$2"
            shift 2
            ;;
        --config-json)
            config_json="$2"
            shift 2
            ;;
        --security-yml)
            security_yml="$2"
            shift 2
            ;;
        --api-key)
            api_key="$2"
            shift 2
            ;;
        --model-name)
            model_name="$2"
            shift 2
            ;;
        --provider)
            provider="$2"
            shift 2
            ;;
        --model)
            model="$2"
            shift 2
            ;;
        --api-base)
            api_base="$2"
            shift 2
            ;;
        --disable-thinking)
            disable_thinking="true"
            shift
            ;;
        --enable-thinking)
            disable_thinking="false"
            shift
            ;;
        --proxy)
            proxy_url="$2"
            shift 2
            ;;
        --env-file)
            env_file="$2"
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
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
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

should_disable_thinking() {
    case "$disable_thinking" in
        true|1|yes|on)
            return 0
            ;;
        false|0|no|off)
            return 1
            ;;
        auto)
            [[ "$model" == "mimo-v2.5" || "$model_name" == "mimo-v25" ]]
            return
            ;;
        *)
            echo "invalid PICOCLAW_DISABLE_THINKING value: ${disable_thinking}" >&2
            exit 2
            ;;
    esac
}

stat_mode() {
    stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

need_cmd cp
need_cmd debugfs
need_cmd install
need_cmd mktemp

"${example_dir}/prepare_picoclaw_assets.sh" --asset-dir "$asset_dir"

ensure_default_rootfs() {
    if [[ ! -f "$default_rootfs" ]]; then
        echo "Default rootfs is missing; building ${default_rootfs}"
        (cd "$workspace" && cargo xtask starry rootfs --arch x86_64)
    fi
    if [[ ! -f "$default_rootfs" ]]; then
        echo "default rootfs does not exist: ${default_rootfs}" >&2
        exit 1
    fi
}

if [[ ! -f "$rootfs" ]]; then
    ensure_default_rootfs
    if [[ "$rootfs" != "$default_rootfs" ]]; then
        mkdir -p "$(dirname "$rootfs")"
        cp --reflink=auto "$default_rootfs" "$rootfs" 2>/dev/null || cp "$default_rootfs" "$rootfs"
    fi
fi

if [[ ! -f "$rootfs" ]]; then
    echo "rootfs does not exist: ${rootfs}" >&2
    exit 1
fi

mkdir -p "$(dirname "$rootfs")"
overlay="$(mktemp -d "${TMPDIR:-/tmp}/picoclaw-rootfs.XXXXXX")"
debug_cmds="$(mktemp "${TMPDIR:-/tmp}/picoclaw-debugfs.XXXXXX")"
tmp_rootfs="$(mktemp "${TMPDIR:-/tmp}/picoclaw-rootfs-img.XXXXXX")"
trap 'rm -rf "$overlay" "$debug_cmds" "$tmp_rootfs"' EXIT
cp --reflink=auto "$rootfs" "$tmp_rootfs" 2>/dev/null || cp "$rootfs" "$tmp_rootfs"

install -d "${overlay}/usr/local/bin"
install -m 0755 "${asset_dir}/picoclaw" "${overlay}/usr/local/bin/picoclaw"
install -m 0755 "${asset_dir}/picoclaw-launcher" "${overlay}/usr/local/bin/picoclaw-launcher"

install -d "${overlay}/root/.picoclaw/workspace"

if [[ -n "$config_json" ]]; then
    install -m 0644 "$config_json" "${overlay}/root/.picoclaw/config.json"
elif [[ -n "$api_key" ]]; then
    extra_body=""
    if should_disable_thinking; then
        extra_body=',
      "extra_body": {
        "chat_template_kwargs": {
          "enable_thinking": false
        }
      }'
    fi
    cat >"${overlay}/root/.picoclaw/config.json" <<EOF
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
      "enabled": true${extra_body}
    }
  ],
  "gateway": {
    "host": "127.0.0.1",
    "port": 18790,
    "hot_reload": false,
    "log_level": "warn"
  }
}
EOF
fi

if [[ -n "$security_yml" ]]; then
    install -m 0600 "$security_yml" "${overlay}/root/.picoclaw/.security.yml"
elif [[ -n "$api_key" ]]; then
    {
        printf 'model_list:\n'
        printf '  %s:\n' "$(yaml_double_quote "$model_name")"
        printf '    api_keys:\n'
        printf '      - %s\n' "$(yaml_double_quote "$api_key")"
    } >"${overlay}/root/.picoclaw/.security.yml"
    chmod 0600 "${overlay}/root/.picoclaw/.security.yml"
fi

if [[ -n "$env_file" ]]; then
    install -m 0600 "$env_file" "${overlay}/root/.picoclaw/starry-online-env"
elif [[ -n "$proxy_url" ]]; then
    {
        printf 'export http_proxy=%s\n' "$(shell_quote "$proxy_url")"
        printf 'export https_proxy=%s\n' "$(shell_quote "$proxy_url")"
        printf 'export all_proxy=%s\n' "$(shell_quote "$proxy_url")"
        printf 'export HTTP_PROXY=%s\n' "$(shell_quote "$proxy_url")"
        printf 'export HTTPS_PROXY=%s\n' "$(shell_quote "$proxy_url")"
        printf 'export ALL_PROXY=%s\n' "$(shell_quote "$proxy_url")"
    } >"${overlay}/root/.picoclaw/starry-online-env"
    chmod 0600 "${overlay}/root/.picoclaw/starry-online-env"
fi

if [[ -n "$ca_cert" && -f "$ca_cert" ]]; then
    install -d "${overlay}/etc/ssl/certs"
    install -m 0644 "$ca_cert" "${overlay}/etc/ssl/certs/ca-certificates.crt"
fi

: >"$debug_cmds"
while IFS= read -r -d '' dir; do
    rel="${dir#${overlay}}"
    [[ -z "$rel" ]] && continue
    printf 'mkdir "%s"\n' "$rel" >>"$debug_cmds"
done < <(find "$overlay" -type d -print0 | sort -z)

while IFS= read -r -d '' file; do
    rel="${file#${overlay}}"
    mode="$(stat_mode "$file")"
    printf 'rm "%s"\n' "$rel" >>"$debug_cmds"
    printf 'write "%s" "%s"\n' "$file" "$rel" >>"$debug_cmds"
    printf 'sif "%s" mode 0100%s\n' "$rel" "$mode" >>"$debug_cmds"
done < <(find "$overlay" -type f -print0 | sort -z)

debugfs -w -f "$debug_cmds" "$tmp_rootfs"
mv "$tmp_rootfs" "$rootfs"

echo "PicoClaw rootfs ready: ${rootfs}"
echo
echo "Offline smoke:"
echo "  cargo xtask starry qemu --arch x86_64 --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml --rootfs ${rootfs#${workspace}/}"
if [[ -n "$api_key" || -n "$config_json" || -n "$security_yml" ]]; then
    echo
    echo "Online agent smoke:"
    echo "  cargo xtask starry qemu --arch x86_64 --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml --rootfs ${rootfs#${workspace}/}"
fi
