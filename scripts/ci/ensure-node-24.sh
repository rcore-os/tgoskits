#!/usr/bin/env bash
# Ensures Node >= 24 (with npm) is available and prints an `export PATH=...`
# line for the caller to eval. Intended use in CI check commands:
#
#     eval "$(bash scripts/ci/ensure-node-24.sh)"
#
# A preinstalled node/npm pair is accepted as-is; otherwise a pinned,
# sha256-verified Node is unpacked once into a persistent cache directory.
# Diagnostics go to stderr so stdout stays eval-able.

set -euo pipefail

node_version="v24.11.1"
node_platform="linux-x64"
node_archive="node-${node_version}-${node_platform}.tar.xz"
node_sha256="60e3b0a8500819514aca603487c254298cd776de0698d3cd08f11dba5b8289a8"
node_root="${HOME}/.cache/tgoskits-node"

node_is_compatible() {
  command -v node >/dev/null 2>&1 \
    && command -v npm >/dev/null 2>&1 \
    && node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 24 ? 0 : 1)'
}

if node_is_compatible; then
  echo "# node $(node --version) / npm $(npm --version) already on PATH" >&2
  exit 0
fi

install_root="${node_root}/node-${node_version}-${node_platform}"
if [ ! -x "${install_root}/bin/node" ]; then
  command -v curl >/dev/null 2>&1 || {
    echo "node/npm >= 24 are unavailable and curl is required to provision Node ${node_version}" >&2
    exit 1
  }
  command -v sha256sum >/dev/null 2>&1 || {
    echo "node/npm >= 24 are unavailable and sha256sum is required to verify Node ${node_version}" >&2
    exit 1
  }
  command -v tar >/dev/null 2>&1 || {
    echo "node/npm >= 24 are unavailable and tar is required to unpack Node ${node_version}" >&2
    exit 1
  }
  mkdir -p "${node_root}"
  curl --fail --location --retry 3 --silent --show-error \
    "https://nodejs.org/dist/${node_version}/${node_archive}" \
    --output "${node_root}/${node_archive}"
  printf '%s  %s\n' "${node_sha256}" "${node_root}/${node_archive}" \
    | sha256sum --check --status -
  rm -rf "${install_root}"
  tar -xJf "${node_root}/${node_archive}" -C "${node_root}"
fi

echo "export PATH=\"${install_root}/bin:\${PATH}\""
echo "# provisioned node ${node_version} at ${install_root}" >&2
