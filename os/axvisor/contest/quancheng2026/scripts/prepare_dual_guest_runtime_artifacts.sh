#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${script_dir}/../../../../.." && pwd)"
archive_url="${QC_RUNTIME_ARTIFACT_URL:-https://raw.githubusercontent.com/irinaparchina-art/tgoskits/contest/quancheng2026-runtime-artifacts/quancheng2026-dual-guest-runtime-v1.tar.xz}"
archive_sha256="${QC_RUNTIME_ARTIFACT_SHA256:-656687bab1f6e055a6be411ee5e4c4a83ccc9366f37c8df9fed0ff5457777283}"
cache_dir=""

usage() {
    cat <<'EOF'
Usage: prepare_dual_guest_runtime_artifacts.sh [options]

Options:
  --repo PATH           tgoskits repository root.
  --archive-url URL     Runtime artifact archive URL.
  --archive-sha256 SHA  Expected archive SHA256.
  --cache-dir PATH      Download cache directory. Default: <repo>/tmp/quancheng2026-runtime-artifacts.
  -h, --help            Show this help.

This script prepares the binary runtime artifacts required by
run_axvisor_dual_guest_qcz1_ai.sh without checking those binaries into the PR.
It downloads a fixed public artifact archive, pulls the reviewed rootfs image
through the checked-in tgosimages registry template, extracts only the expected
archive members, and verifies runtime-artifacts-known-passing.sha256.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo)
            repo="$2"
            shift 2
            ;;
        --archive-url)
            archive_url="$2"
            shift 2
            ;;
        --archive-sha256)
            archive_sha256="$2"
            shift 2
            ;;
        --cache-dir)
            cache_dir="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

repo="$(cd "${repo}" && pwd)"
contest_dir="${repo}/os/axvisor/contest/quancheng2026"
rootfs_cache="${repo}/tmp/axbuild/rootfs"
rootfs_registry="${rootfs_cache}/images.toml"
rootfs_registry_source="${contest_dir}/runtime-rootfs-images-known-passing.toml"
rootfs_image="${rootfs_cache}/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img"
artifact_manifest="${contest_dir}/runtime-artifacts-known-passing.sha256"
if [[ -z "${cache_dir}" ]]; then
    cache_dir="${repo}/tmp/quancheng2026-runtime-artifacts"
fi
archive_path="${cache_dir}/quancheng2026-dual-guest-runtime-v1.tar.xz"

for required in curl sha256sum tar cargo; do
    if ! command -v "${required}" >/dev/null 2>&1; then
        echo "missing_required_tool=${required}" >&2
        exit 10
    fi
done
for required_path in "${rootfs_registry_source}" "${artifact_manifest}"; do
    if [[ ! -f "${required_path}" ]]; then
        echo "missing_required_path=${required_path}" >&2
        exit 11
    fi
done

mkdir -p "${rootfs_cache}" "${cache_dir}"
cp "${rootfs_registry_source}" "${rootfs_registry}"
date +%s >"${rootfs_cache}/.last_sync"

if [[ ! -f "${rootfs_image}" ]]; then
    echo "rootfs_image_missing=${rootfs_image}"
    echo "action=cargo xtask image --no-auto-sync -S tmp/axbuild/rootfs pull --arch aarch64"
    (cd "${repo}" && cargo xtask image --no-auto-sync -S tmp/axbuild/rootfs pull --arch aarch64)
fi
if [[ ! -f "${rootfs_image}" ]]; then
    echo "missing_required_path=${rootfs_image}" >&2
    exit 12
fi

need_download=1
if [[ -f "${archive_path}" ]]; then
    current_sha="$(sha256sum "${archive_path}" | awk '{print $1}')"
    if [[ "${current_sha}" == "${archive_sha256}" ]]; then
        need_download=0
    fi
fi
if [[ "${need_download}" -eq 1 ]]; then
    tmp_archive="${archive_path}.tmp"
    rm -f "${tmp_archive}"
    curl -4 --retry 5 --retry-delay 2 -fL "${archive_url}" -o "${tmp_archive}"
    mv "${tmp_archive}" "${archive_path}"
fi

printf '%s  %s\n' "${archive_sha256}" "${archive_path}" | sha256sum --check -

expected_members="os/axvisor/tmp/configs/2026-07-24_qemu-aarch64-host-reserve-zephyr-0x90000000.dtb
os/axvisor/tmp/images/qemu-aarch64/linux/linux-qemu
os/axvisor/tmp/images/qemu-aarch64/zephyr-e1000-0x90000000-qcz1/zephyr.bin"
actual_members="$(tar -tf "${archive_path}" | LC_ALL=C sort)"
if [[ "${actual_members}" != "${expected_members}" ]]; then
    echo "runtime_artifact_archive_members=FAIL" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "${expected_members}" "${actual_members}" >&2
    exit 13
fi

tar -xJf "${archive_path}" -C "${repo}"

echo "runtime_artifact_archive=${archive_path}"
echo "runtime_artifact_archive_url=${archive_url}"
echo "runtime_artifact_archive_sha256=${archive_sha256}"
echo "runtime_artifact_archive_members=PASS"
(cd "${repo}" && sha256sum --strict --check "${artifact_manifest}")
echo "runtime_artifact_manifest_check=PASS"
