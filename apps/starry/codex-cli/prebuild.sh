#!/usr/bin/env bash
set -euo pipefail

workspace="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
app_dir="${STARRY_APP_DIR:-$workspace/apps/starry/codex-cli}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
asset_dir="$workspace/target/codex/assets"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/starry-codex-assets.XXXXXX")"
package="@openai/codex@0.115.0-linux-x64"

codex_sha256="440269f35afeb90d38115af844629d98705fb7266fdcd5fe7c040a78ebc75b85"
rg_sha256="ebeaf56f8a25e102e9419933423738b3a2a613a444fd749d695e15eba53f71f2"

auth_json="${CODEX_AUTH_JSON:-}"
proxy_url="${CODEX_ONLINE_PROXY:-}"
env_file="${CODEX_ONLINE_ENV_FILE:-}"
ca_cert="${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}"

cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

ensure_host_packages() {
    local missing=()

    command -v npm >/dev/null 2>&1 || missing+=(npm)
    command -v tar >/dev/null 2>&1 || missing+=(tar)
    command -v sha256sum >/dev/null 2>&1 || missing+=(coreutils)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)

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

verify_sha256() {
    local expected="$1"
    local path="$2"
    local actual

    actual="$(sha256sum "$path" | awk '{print $1}')"
    if [[ "$actual" != "$expected" ]]; then
        echo "error: SHA-256 mismatch for $path" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
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

install_codex_assets() {
    ensure_host_packages

    mkdir -p "$asset_dir"

    echo "Preparing Codex CLI assets from npm package $package"
    local pack_output tarball codex_src rg_src
    pack_output="$(npm pack "$package" --pack-destination "$tmp_dir" --silent)"
    tarball="$(printf '%s\n' "$pack_output" | tail -n 1)"
    tar -xzf "$tmp_dir/$tarball" -C "$tmp_dir"

    codex_src="$tmp_dir/package/vendor/x86_64-unknown-linux-musl/codex/codex"
    rg_src="$tmp_dir/package/vendor/x86_64-unknown-linux-musl/path/rg"

    if [[ ! -f "$codex_src" || ! -f "$rg_src" ]]; then
        echo "error: expected Codex or ripgrep binary missing from $package" >&2
        exit 1
    fi

    install -m 0755 "$codex_src" "$asset_dir/codex"
    install -m 0755 "$rg_src" "$asset_dir/rg"

    verify_sha256 "$codex_sha256" "$asset_dir/codex"
    verify_sha256 "$rg_sha256" "$asset_dir/rg"

    "$asset_dir/codex" --version
    "$asset_dir/rg" --version | head -n 1

    echo "Codex CLI assets ready in $asset_dir"
}

populate_overlay() {
    if [[ -z "$overlay_dir" ]]; then
        echo "error: STARRY_OVERLAY_DIR is required" >&2
        exit 1
    fi

    local codex_bin="$asset_dir/codex"
    local rg_bin="$asset_dir/rg"
    if [[ ! -x "$codex_bin" || ! -x "$rg_bin" ]]; then
        echo "error: Codex assets are missing after asset preparation" >&2
        exit 1
    fi

    install -Dm0755 "$codex_bin" "$overlay_dir/usr/local/bin/codex"
    install -Dm0755 "$rg_bin" "$overlay_dir/usr/local/bin/rg"

    if [[ -n "$auth_json" ]]; then
        auth_json="$(workspace_path "$auth_json")"
        if [[ ! -f "$auth_json" ]]; then
            echo "error: auth.json not found: $auth_json" >&2
            echo "       pass CODEX_AUTH_JSON or omit auth for offline examples" >&2
            exit 1
        fi
        install -Dm0600 "$auth_json" "$overlay_dir/root/.codex/auth.json"
    fi

    if [[ -n "$env_file" ]]; then
        env_file="$(workspace_path "$env_file")"
        if [[ ! -f "$env_file" ]]; then
            echo "error: env file not found: $env_file" >&2
            exit 1
        fi
        install -Dm0644 "$env_file" "$overlay_dir/root/.codex/starry-online-env"
    elif [[ -n "$proxy_url" ]]; then
        mkdir -p "$overlay_dir/root/.codex"
        local quoted_proxy
        quoted_proxy="$(shell_quote "$proxy_url")"
        cat > "$overlay_dir/root/.codex/starry-online-env" <<ENVEOF
export HTTP_PROXY=$quoted_proxy
export HTTPS_PROXY=$quoted_proxy
export ALL_PROXY=$quoted_proxy
export http_proxy=$quoted_proxy
export https_proxy=$quoted_proxy
export all_proxy=$quoted_proxy
export NO_PROXY='localhost,127.0.0.1,::1'
export no_proxy='localhost,127.0.0.1,::1'
ENVEOF
        chmod 0644 "$overlay_dir/root/.codex/starry-online-env"
    fi

    ca_cert="$(workspace_path "$ca_cert")"
    if [[ -f "$ca_cert" ]]; then
        install -Dm0644 "$ca_cert" "$overlay_dir/etc/ssl/certs/ca-certificates.crt"
    else
        echo "warning: CA bundle not found at $ca_cert; relying on the base rootfs CA bundle" >&2
    fi
}

main() {
    # Keep the app directory variable available for future app-local assets.
    : ""

    if [[ -z "$overlay_dir" ]]; then
        echo "error: STARRY_OVERLAY_DIR is required" >&2
        exit 1
    fi

    install_codex_assets
    populate_overlay
}

main "$@"
