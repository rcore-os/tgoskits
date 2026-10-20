#!/usr/bin/env bash
# prebuild.sh - cache and verify the pinned inspect fixtures, publish the source
# tarball to the host HTTP document root, and stage the rockspec plus the guest
# scripts into the overlay.
#
# The app has a prebuild, so the runner selects the plain pipeline. That means
# the sh/ files are not copied by the generic shell asset pipeline and must be
# installed here explicitly.
set -euo pipefail

app_dir="${STARRY_APP_DIR:?prebuild: STARRY_APP_DIR required}"
workspace="${STARRY_WORKSPACE:?prebuild: STARRY_WORKSPACE required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

rockspec_url='https://luarocks.org/inspect-3.1.3-0.rockspec'
rockspec_sha256='dac6f2b2362abe381daaa92448fc3518752937c75e57c15465ea825fa5583bc6'
tarball_url='https://github.com/kikito/inspect.lua/archive/v3.1.3.tar.gz'
tarball_sha256='43386a613f9916186af4919f36392d8b8dad8c4428a5f8c85ebac80acba71eeb'

source_archive='inspect.lua-3.1.3.tar.gz'
source_dir='inspect.lua-3.1.3'
rockspec_name='inspect-3.1.3-0.rockspec'

# Fixed port of the [host_http_server] entry in qemu-riscv64.toml. The guest
# reaches the host through QEMU SLIRP at 10.0.2.2.
host_port=18391
guest_source_url="http://10.0.2.2:${host_port}/${source_archive}"

assets_dir="$workspace/target/lua-luarocks-assets"
rockspec_cache="$assets_dir/$rockspec_name"
tarball_cache="$assets_dir/$source_archive"

fail() {
    echo "prebuild: $*" >&2
    exit 1
}

for tool in curl sha256sum sed grep wc install; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing host tool: $tool"
done

mkdir -p "$assets_dir"

# A cache hit is still verified on every run. A mismatching cache is rejected
# instead of being silently replaced, so a corrupt or substituted file cannot
# reach the guest.
fetch_verified() {
    local url="$1" dest="$2" want="$3" part="$2.part"

    if [[ -f "$dest" ]]; then
        printf '%s  %s\n' "$want" "$dest" | sha256sum -c - >/dev/null \
            || fail "cached $(basename "$dest") does not match the pinned SHA-256"
        echo "prebuild: verified cached $(basename "$dest")"
        return 0
    fi

    rm -f "$part"
    curl --fail --location --connect-timeout 20 --max-time 120 \
        --output "$part" "$url" \
        || fail "download failed: $url"
    printf '%s  %s\n' "$want" "$part" | sha256sum -c - >/dev/null \
        || { rm -f "$part"; fail "downloaded $(basename "$dest") does not match the pinned SHA-256"; }
    mv -f "$part" "$dest"
    echo "prebuild: fetched $(basename "$dest")"
}

fetch_verified "$rockspec_url" "$rockspec_cache" "$rockspec_sha256"
fetch_verified "$tarball_url" "$tarball_cache" "$tarball_sha256"

# The pinned SHA-256 fixes the whole rockspec. Count actual URL occurrences,
# not whole matching lines: the upstream file writes `  url = "<url>",`.
[[ "$(grep -Fo "$tarball_url" "$rockspec_cache" | wc -l)" == 1 ]] \
    || fail "rockspec must contain the pinned source URL exactly once"

rockspec_staged="$overlay_dir/usr/share/lua-luarocks/$rockspec_name"
mkdir -p "$(dirname "$rockspec_staged")"
sed "s|$tarball_url|$guest_source_url|" "$rockspec_cache" >"$rockspec_staged"

grep -Fq "url = \"$guest_source_url\"" "$rockspec_staged" \
    || fail "rewritten rockspec does not point at $guest_source_url"
if grep -Fq "$tarball_url" "$rockspec_staged"; then
    fail "rewritten rockspec still contains the GitHub source URL"
fi

install -Dm0755 "$app_dir/sh/lua-luarocks-tests.sh" \
    "$overlay_dir/usr/bin/lua-luarocks-tests.sh"
install -Dm0644 "$app_dir/sh/lua-luarocks-main.lua" \
    "$overlay_dir/usr/bin/lua-luarocks-main.lua"

echo "prebuild: staged inspect $source_dir; host HTTP root $assets_dir"
