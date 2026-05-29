#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
C_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)

find_repo_root() {
    local dir=$1
    while [[ "${dir}" != "/" ]]; do
        if [[ -f "${dir}/Cargo.toml" && -d "${dir}/test-suit" ]]; then
            printf '%s\n' "${dir}"
            return 0
        fi
        dir=$(dirname "${dir}")
    done
    return 1
}

REPO_ROOT=$(find_repo_root "${C_DIR}")

PACKAGE=${K230_KMODEL_PACKAGE:-kmodel_v2.1.0.tgz}
MODEL_NAME=${K230_KMODEL_NAME:-yolov8n_320.kmodel}
BASE_URL=${K230_KMODEL_BASE_URL:-https://kendryte-download.canaan-creative.com/k230/downloads/kmodel}
CACHE_DIR=${K230_KMODEL_CACHE_DIR:-${REPO_ROOT}/target/k230-kmodels}
ASSET_DIR=${K230_KMODEL_ASSET_DIR:-${C_DIR}/assets/kmodels}

ARCHIVE="${CACHE_DIR}/${PACKAGE}"
EXTRACT_DIR="${CACHE_DIR}/${PACKAGE%.tgz}"
URL="${BASE_URL}/${PACKAGE}"

mkdir -p "${CACHE_DIR}" "${EXTRACT_DIR}" "${ASSET_DIR}"

if [[ ! -f "${ARCHIVE}" ]]; then
    curl -L --fail --continue-at - --output "${ARCHIVE}" "${URL}"
fi

if [[ -z "$(find "${EXTRACT_DIR}" -type f -name "${MODEL_NAME}" -print -quit)" ]]; then
    tar -xzf "${ARCHIVE}" -C "${EXTRACT_DIR}"
fi

MODEL_SOURCE=$(find "${EXTRACT_DIR}" -type f -name "${MODEL_NAME}" -print -quit)
if [[ -z "${MODEL_SOURCE}" ]]; then
    echo "model ${MODEL_NAME} was not found in ${ARCHIVE}" >&2
    exit 1
fi

install -m 0644 "${MODEL_SOURCE}" "${ASSET_DIR}/${MODEL_NAME}"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "${ASSET_DIR}" && sha256sum "${MODEL_NAME}" > "${MODEL_NAME}.sha256")
elif command -v shasum >/dev/null 2>&1; then
    (cd "${ASSET_DIR}" && shasum -a 256 "${MODEL_NAME}" > "${MODEL_NAME}.sha256")
else
    echo "sha256 tool not found; installed model without checksum sidecar" >&2
fi

echo "installed ${MODEL_NAME} to ${ASSET_DIR}"
