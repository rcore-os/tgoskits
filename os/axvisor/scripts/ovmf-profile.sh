#!/usr/bin/env bash

# Fixed x86_64 guest firmware contract used by AxVisor entry diagnostics.
# This file is sourced by setup_qemu.sh and quick-start.sh.

OVMF_PROFILE_NAME="qemu_x86_64_axvisor_ovmf_debug"
OVMF_EDK2_TAG="edk2-stable202605"
OVMF_EDK2_COMMIT="b03a21a63e3bd001f52c527e5a57feddb53a690b"
OVMF_CODE_BASE=0xffc84000
OVMF_CODE_SIZE=0x37c000
OVMF_VARS_BASE=0xffc00000
OVMF_VARS_SIZE=0x84000
OVMF_COMBINED_SIZE=0x400000
OVMF_RESET_VECTOR=0xfffffff0

ovmf_prepare_firmware() {
    local repo_root="$1"
    local image_output_root="$2"
    local external_firmware="${AXVISOR_X86_64_UEFI_FIRMWARE:-}"
    local allow_unverified="${AXVISOR_X86_64_UEFI_ALLOW_UNVERIFIED:-0}"

    if [ -n "${external_firmware}" ]; then
        if [ "${allow_unverified}" != "1" ]; then
            ovmf_error \
                "AXVISOR_X86_64_UEFI_FIRMWARE requires AXVISOR_X86_64_UEFI_ALLOW_UNVERIFIED=1"
            return 1
        fi
        ovmf_prepare_unverified_firmware "${external_firmware}"
        return
    fi

    local bundle_dir="${image_output_root}/${OVMF_PROFILE_NAME}"
    if [ ! -d "${bundle_dir}" ]; then
        mkdir -p "${image_output_root}"
        echo "  -> Pulling verified OVMF profile ${OVMF_PROFILE_NAME} from tgosimages..."
        (
            cd "${repo_root}"
            cargo axvisor image pull "${OVMF_PROFILE_NAME}" --output-dir "${image_output_root}"
        )
    fi

    ovmf_verify_bundle "${bundle_dir}"
}

ovmf_verify_bundle() {
    local bundle_dir="$1"
    local manifest="${bundle_dir}/manifest.toml"

    [ -f "${manifest}" ] || {
        ovmf_error "verified OVMF bundle is missing ${manifest}"
        return 1
    }

    ovmf_validate_flat_manifest "${manifest}" || return 1
    ovmf_require_manifest_value "${manifest}" schema_version "1"
    ovmf_require_manifest_value "${manifest}" profile "${OVMF_PROFILE_NAME}"
    ovmf_require_manifest_value "${manifest}" edk2_tag "${OVMF_EDK2_TAG}"
    ovmf_require_manifest_value "${manifest}" edk2_commit "${OVMF_EDK2_COMMIT}"
    ovmf_require_manifest_value "${manifest}" architecture "X64"
    ovmf_require_manifest_value "${manifest}" target "DEBUG"
    ovmf_require_manifest_value "${manifest}" toolchain "GCC"
    ovmf_require_manifest_value "${manifest}" platform "OvmfPkg/OvmfPkgX64.dsc"
    ovmf_require_manifest_number "${manifest}" code_base "${OVMF_CODE_BASE}"
    ovmf_require_manifest_number "${manifest}" code_size "${OVMF_CODE_SIZE}"
    ovmf_require_manifest_number "${manifest}" vars_base "${OVMF_VARS_BASE}"
    ovmf_require_manifest_number "${manifest}" vars_size "${OVMF_VARS_SIZE}"
    ovmf_require_manifest_number "${manifest}" combined_size "${OVMF_COMBINED_SIZE}"
    ovmf_require_manifest_number "${manifest}" reset_vector "${OVMF_RESET_VECTOR}"
    ovmf_require_manifest_value "${manifest}" code_file "OVMF_CODE.fd"
    ovmf_require_manifest_value "${manifest}" vars_file "OVMF_VARS.fd"
    ovmf_require_manifest_value "${manifest}" combined_file "OVMF.fd"
    ovmf_require_manifest_value "${manifest}" fd_size_4mb "true"
    ovmf_require_manifest_value "${manifest}" debug_on_serial_port "true"
    ovmf_require_manifest_value "${manifest}" build_shell "true"
    ovmf_require_manifest_value "${manifest}" smm_require "false"
    ovmf_require_manifest_value "${manifest}" secure_boot_enable "false"
    ovmf_require_manifest_value "${manifest}" tpm2_enable "false"
    ovmf_require_manifest_value "${manifest}" network_enable "false"
    ovmf_require_manifest_value "${manifest}" sdcard_enable "false"
    ovmf_require_manifest_value "${manifest}" cc_measurement_enable "false"
    ovmf_require_manifest_value "${manifest}" sec_marker "SecCoreStartupWithStack("
    ovmf_require_manifest_value "${manifest}" pei_marker "Platform PEIM Loaded"
    ovmf_require_manifest_value "${manifest}" dxe_ipl_marker "DXE IPL Entry"
    ovmf_require_manifest_value "${manifest}" dxe_core_marker "Loading DXE CORE at"
    ovmf_require_manifest_value "${manifest}" bds_marker "[BdsDxe]"

    ovmf_require_manifest_field "${manifest}" build_command
    ovmf_require_manifest_field "${manifest}" build_container_digest
    ovmf_require_manifest_field "${manifest}" tool_versions
    ovmf_require_manifest_field "${manifest}" submodule_commits

    local code_path="${bundle_dir}/OVMF_CODE.fd"
    local vars_path="${bundle_dir}/OVMF_VARS.fd"
    local combined_path="${bundle_dir}/OVMF.fd"
    ovmf_verify_manifest_file "${manifest}" code "${code_path}" "${OVMF_CODE_SIZE}"
    ovmf_verify_manifest_file "${manifest}" vars "${vars_path}" "${OVMF_VARS_SIZE}"
    ovmf_verify_manifest_file "${manifest}" combined "${combined_path}" "${OVMF_COMBINED_SIZE}"

    if ! cmp -s <(command cat "${vars_path}" "${code_path}") "${combined_path}"; then
        ovmf_error "OVMF.fd is not the byte-for-byte concatenation of OVMF_VARS.fd and OVMF_CODE.fd"
        return 1
    fi

    local code_end=$((OVMF_CODE_BASE + OVMF_CODE_SIZE))
    if (( OVMF_RESET_VECTOR < OVMF_CODE_BASE || OVMF_RESET_VECTOR + 16 > code_end )); then
        ovmf_error "reset vector is outside the fixed OVMF CODE window"
        return 1
    fi

    OVMF_CODE_PATH="${code_path}"
    OVMF_CODE_SHA256="$(ovmf_sha256 "${code_path}")"
    OVMF_VERIFICATION_LABEL="VERIFIED"
    export OVMF_CODE_PATH OVMF_CODE_SHA256 OVMF_VERIFICATION_LABEL
    ovmf_print_selected_firmware
}

ovmf_prepare_unverified_firmware() {
    local firmware="$1"
    [ -f "${firmware}" ] || {
        ovmf_error "unverified UEFI firmware does not exist: ${firmware}"
        return 1
    }

    local actual_size
    actual_size="$(ovmf_file_size "${firmware}")"
    if (( actual_size != OVMF_CODE_SIZE )); then
        ovmf_error \
            "unverified UEFI firmware must still match the fixed CODE size $((OVMF_CODE_SIZE)) bytes; got ${actual_size}"
        return 1
    fi

    OVMF_CODE_PATH="${firmware}"
    OVMF_CODE_SHA256="$(ovmf_sha256 "${firmware}")"
    OVMF_VERIFICATION_LABEL="UNVERIFIED"
    export OVMF_CODE_PATH OVMF_CODE_SHA256 OVMF_VERIFICATION_LABEL
    ovmf_print_selected_firmware
    echo "  -> WARNING: UNVERIFIED firmware is diagnostic-only and must not determine UEFI test results." >&2
}

ovmf_validate_flat_manifest() {
    local manifest="$1"
    local problem

    # The verifier intentionally accepts only the flat subset it can parse:
    # bare keys and single-line string, boolean, or unsigned integer values.
    problem="$(awk '
        /^[[:space:]]*($|#)/ {
            next
        }
        {
            line = $0
            if (line !~ /^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=/) {
                print "syntax:" NR
                exit
            }

            key = line
            sub(/^[[:space:]]*/, "", key)
            sub(/[[:space:]]*=.*$/, "", key)
            if (++seen[key] > 1) {
                print "duplicate:" key
                exit
            }

            value = line
            sub(/^[^=]*=[[:space:]]*/, "", value)
            if (value ~ /^"[^"]*"[[:space:]]*(#.*)?$/) {
                next
            }
            if (value ~ /^(true|false|0[xX][0-9a-fA-F]+|[0-9]+)[[:space:]]*(#.*)?$/) {
                next
            }

            print "syntax:" NR
            exit
        }
    ' "${manifest}")"
    case "${problem}" in
        duplicate:*)
            ovmf_error "OVMF manifest contains duplicate top-level key: ${problem#duplicate:}"
            return 1
            ;;
        syntax:*)
            ovmf_error \
                "OVMF manifest line ${problem#syntax:} is outside the supported flat bare-key scalar syntax"
            return 1
            ;;
    esac
}

ovmf_verify_manifest_file() {
    local manifest="$1"
    local role="$2"
    local path="$3"
    local expected_size="$4"
    local expected_hash

    [ -f "${path}" ] || {
        ovmf_error "OVMF bundle is missing ${path}"
        return 1
    }
    expected_hash="$(ovmf_manifest_value "${manifest}" "${role}_sha256")"
    [ -n "${expected_hash}" ] || {
        ovmf_error "OVMF manifest is missing ${role}_sha256"
        return 1
    }
    if ! printf '%s\n' "${expected_hash}" | grep -Eq '^[0-9a-f]{64}$'; then
        ovmf_error "OVMF manifest has an invalid ${role}_sha256: ${expected_hash}"
        return 1
    fi

    local actual_size
    actual_size="$(ovmf_file_size "${path}")"
    if (( actual_size != expected_size )); then
        ovmf_error "${path} has size ${actual_size}; expected $((expected_size))"
        return 1
    fi

    local actual_hash
    actual_hash="$(ovmf_sha256 "${path}")"
    if [ "${actual_hash}" != "${expected_hash}" ]; then
        ovmf_error "SHA-256 mismatch for ${path}: expected ${expected_hash}, got ${actual_hash}"
        return 1
    fi
}

ovmf_require_manifest_number() {
    local manifest="$1"
    local key="$2"
    local expected="$3"
    local actual

    actual="$(ovmf_manifest_value "${manifest}" "${key}")"
    [ -n "${actual}" ] || {
        ovmf_error "OVMF manifest is missing ${key}"
        return 1
    }
    if ! printf '%s\n' "${actual}" | grep -Eq '^(0[xX][0-9a-fA-F]+|[0-9]+)$'; then
        ovmf_error "OVMF manifest ${key} is not an unsigned integer: ${actual}"
        return 1
    fi
    if (( actual != expected )); then
        ovmf_error "OVMF manifest ${key}=${actual}; expected $(printf '0x%x' "${expected}")"
        return 1
    fi
}

ovmf_require_manifest_value() {
    local manifest="$1"
    local key="$2"
    local expected="$3"
    local actual

    actual="$(ovmf_manifest_value "${manifest}" "${key}")"
    if [ "${actual}" != "${expected}" ]; then
        ovmf_error "OVMF manifest ${key}=${actual:-<missing>}; expected ${expected}"
        return 1
    fi
}

ovmf_require_manifest_field() {
    local manifest="$1"
    local key="$2"
    local actual

    actual="$(ovmf_manifest_value "${manifest}" "${key}")"
    if [ -z "${actual}" ]; then
        ovmf_error "OVMF manifest is missing required provenance field ${key}"
        return 1
    fi
}

ovmf_manifest_value() {
    local manifest="$1"
    local key="$2"

    awk -v key="${key}" '
        $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            value = $0
            sub("^[[:space:]]*" key "[[:space:]]*=[[:space:]]*", "", value)
            sub(/[[:space:]]*#[^"]*$/, "", value)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            if (value ~ /^".*"$/) {
                sub(/^"/, "", value)
                sub(/"$/, "", value)
            }
            print value
            exit
        }
    ' "${manifest}"
}

ovmf_sha256() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "${file}" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "${file}" | awk '{print $1}'
    else
        ovmf_error "sha256sum or shasum is required to verify OVMF firmware"
        return 1
    fi
}

ovmf_file_size() {
    local file="$1"
    if stat -c '%s' "${file}" >/dev/null 2>&1; then
        stat -c '%s' "${file}"
    else
        stat -f '%z' "${file}"
    fi
}

ovmf_print_selected_firmware() {
    printf '  -> OVMF profile: %s (%s)\n' "${OVMF_PROFILE_NAME}" "${OVMF_VERIFICATION_LABEL}"
    printf '  -> OVMF CODE: %s\n' "${OVMF_CODE_PATH}"
    printf '  -> OVMF CODE SHA-256: %s\n' "${OVMF_CODE_SHA256}"
    printf '  -> OVMF CODE mapping: 0x%x..0x%x (%d bytes), reset=0x%x\n' \
        "${OVMF_CODE_BASE}" "$((OVMF_CODE_BASE + OVMF_CODE_SIZE - 1))" \
        "${OVMF_CODE_SIZE}" "${OVMF_RESET_VECTOR}"
}

ovmf_error() {
    echo "ERROR: $*" >&2
}
