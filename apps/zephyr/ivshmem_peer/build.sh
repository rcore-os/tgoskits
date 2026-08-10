#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd "${script_dir}/../../.." && pwd)"
axbuild_zephyr_dir="${workspace_root}/tmp/axbuild/zephyr"

zephyr_repo_url="${ZEPHYR_REPO_URL:-https://github.com/zephyrproject-rtos/zephyr.git}"
zephyr_ref="${ZEPHYR_REF:-30bef2a126198f73ecc1f8a90590579e03379b18}"
zephyr_src_dir="${ZEPHYR_SRC_DIR:-${axbuild_zephyr_dir}/src}"
zephyr_sdk_url="${ZEPHYR_SDK_URL:-https://github.com/zephyrproject-rtos/sdk-ng/releases/download/v1.0.1/toolchain_gnu_linux-x86_64_aarch64-zephyr-elf.tar.xz}"
zephyr_sdk_dir="${ZEPHYR_SDK_DIR:-${axbuild_zephyr_dir}/sdk}"
zephyr_pyenv="${ZEPHYR_PYENV:-${axbuild_zephyr_dir}/pyenv}"
zephyr_python="${ZEPHYR_PYTHON:-${zephyr_pyenv}/bin/python}"
cross_compile="${ZEPHYR_CROSS_COMPILE:-${zephyr_sdk_dir}/bin/aarch64-zephyr-elf-}"
build_dir="${AXVISOR_ZEPHYR_IVSHMEM_BUILD_DIR:-${axbuild_zephyr_dir}/ivshmem_peer}"
out_dir="${AXVISOR_ZEPHYR_IVSHMEM_OUT_DIR:-${build_dir}/out}"

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 2
    fi
}

ensure_zephyr_source() {
    if [[ ! -d "${zephyr_src_dir}/.git" ]]; then
        mkdir -p "$(dirname "${zephyr_src_dir}")"
        git clone "${zephyr_repo_url}" "${zephyr_src_dir}"
    fi
    git -C "${zephyr_src_dir}" fetch --tags --quiet origin "${zephyr_ref}" || true
    git -C "${zephyr_src_dir}" checkout --quiet "${zephyr_ref}"
}

ensure_zephyr_sdk() {
    if [[ -x "${cross_compile}gcc" ]]; then
        return 0
    fi

    local tmpdir
    tmpdir="$(mktemp -d)"
    curl -fSL -o "${tmpdir}/zephyr-sdk.tar.xz" "${zephyr_sdk_url}"
    mkdir -p "${zephyr_sdk_dir}"
    tar xf "${tmpdir}/zephyr-sdk.tar.xz" -C "${zephyr_sdk_dir}" --strip-components=1
    rm -rf "${tmpdir}"

    if [[ ! -x "${cross_compile}gcc" ]]; then
        echo "Zephyr SDK compiler not found: ${cross_compile}gcc" >&2
        exit 1
    fi
}

ensure_zephyr_python() {
    if [[ ! -x "${zephyr_python}" ]]; then
        python3 -m venv "${zephyr_pyenv}"
    fi
    if ! "${zephyr_python}" -c 'import pykwalify, yaml, west' >/dev/null 2>&1; then
        "${zephyr_pyenv}/bin/pip" install -r "${zephyr_src_dir}/scripts/requirements-base.txt"
    fi
    if [[ ! -d "${axbuild_zephyr_dir}/.west" ]]; then
        "${zephyr_pyenv}/bin/west" init -l "${zephyr_src_dir}" --mf west.yml
    fi
}

prepare_module_metadata() {
    mkdir -p "${build_dir}/Kconfig"
    "${zephyr_python}" "${zephyr_src_dir}/scripts/zephyr_module.py" \
        --kconfig-out "${build_dir}/Kconfig/Kconfig.modules" \
        --sysbuild-kconfig-out "${build_dir}/Kconfig/Kconfig.sysbuild.modules" \
        --cmake-out "${build_dir}/zephyr_modules.txt" \
        --settings-out "${build_dir}/zephyr_settings.txt" \
        -z "${zephyr_src_dir}"
}

need_cmd git
need_cmd cmake
need_cmd ninja
need_cmd curl
need_cmd tar
need_cmd python3

ensure_zephyr_source
ensure_zephyr_sdk
ensure_zephyr_python

if [[ -f "${build_dir}/CMakeCache.txt" ]]; then
    cached_home="$(sed -n 's/^CMAKE_HOME_DIRECTORY:INTERNAL=//p' "${build_dir}/CMakeCache.txt" | tail -n 1)"
    if [[ -n "${cached_home}" && "${cached_home}" != "${script_dir}" ]]; then
        rm -rf "${build_dir}"
    fi
fi

prepare_module_metadata
mkdir -p "${out_dir}"

export ZEPHYR_BASE="${zephyr_src_dir}"
export CROSS_COMPILE="${cross_compile}"
export CCACHE_DISABLE=1

cmake \
    -GNinja \
    -B "${build_dir}" \
    -S "${script_dir}" \
    -DBOARD=qemu_cortex_a53 \
    -DZEPHYR_TOOLCHAIN_VARIANT=cross-compile \
    -DPython3_EXECUTABLE="${zephyr_python}"

cmake --build "${build_dir}" -j"$(nproc)"

cp -f "${build_dir}/zephyr/zephyr.bin" "${out_dir}/zephyr-ivshmem-peer.bin"
cp -f "${build_dir}/zephyr/zephyr.elf" "${out_dir}/zephyr-ivshmem-peer.elf"

echo "AXVISOR_ZEPHYR_IVSHMEM_OUT_DIR=${out_dir}"
