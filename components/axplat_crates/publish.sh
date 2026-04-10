#!/bin/bash

# Publish script for axplat crates
# This script publishes crates in the correct dependency order

set -e  # Exit on error

echo "=========================================="
echo "Publishing axplat crates..."
echo "=========================================="

# Define targets for different architectures
AARCH64_TARGET="aarch64-unknown-none-softfloat"
X86_64_TARGET="x86_64-unknown-none"
RISCV64_TARGET="riscv64gc-unknown-none-elf"
LOONGARCH64_TARGET="loongarch64-unknown-none"

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Function to get local crate version from Cargo.toml
# Handles both direct version and workspace = true
get_local_version() {
    local crate_path=$1
    local version_line
    # Only match direct version assignment (version = "..."), not version.workspace = true
    version_line=$(grep -E '^version\s*=\s*"' "${crate_path}/Cargo.toml" | head -1)

    if [ -n "$version_line" ]; then
        # Direct version: version = "0.1.0"
        echo "$version_line" | sed -E 's/.*=\s*"([^"]+)".*/\1/'
    else
        # Workspace version: version.workspace = true
        # Get from workspace Cargo.toml (always in project root)
        grep -E '^version\s*=' "Cargo.toml" | head -1 | sed -E 's/.*=\s*"([^"]+)".*/\1/'
    fi
}

# Function to check if a crate version already exists on crates.io
crate_version_exists() {
    local crate_name=$1
    local version=$2

    # Search for the crate on crates.io and check if the version exists
    local search_result
    search_result=$(cargo search "${crate_name}" --limit 1 2>/dev/null | grep "^${crate_name} = \"${version}\"") || true

    if [ -n "${search_result}" ]; then
        return 0  # Version exists
    else
        return 1  # Version does not exist
    fi
}

# Function to publish a crate
publish_crate() {
    local crate_name=$1
    local crate_path=$2
    local target=$3
    local version

    # Get the local version from Cargo.toml
    version=$(get_local_version "${crate_path}")

    echo ""
    echo -e "${BLUE}Checking ${crate_name} (version ${version})...${NC}"

    # Check if this version already exists on crates.io
    if crate_version_exists "${crate_name}" "${version}"; then
        echo -e "${YELLOW}Skipping ${crate_name} ${version} - already published on crates.io${NC}"
        return 0
    fi

    echo -e "${BLUE}Publishing ${crate_name} ${version}...${NC}"

    if [ -n "$target" ]; then
        cargo publish -p "${crate_name}" --target "${target}"
    else
        cargo publish -p "${crate_name}"
    fi

    if [ $? -eq 0 ]; then
        echo -e "${GREEN}Successfully published ${crate_name} ${version}${NC}"
    else
        echo -e "${RED}Failed to publish ${crate_name}${NC}"
        exit 1
    fi

    # Wait a bit for the crate to be available on crates.io
    echo "Waiting for ${crate_name} to be available on crates.io..."
    sleep 10
}

echo ""
echo "Step 1: Publishing base crates (no special target required)..."
echo "------------------------------------------------------------"

# 1. Publish axplat-macros first (no dependencies)
publish_crate "axplat-macros" "axplat-macros" ""

# 2. Publish axplat (depends on axplat-macros)
publish_crate "axplat" "axplat" ""

echo ""
echo "Step 2: Publishing platform-specific crates with appropriate targets..."
echo "-----------------------------------------------------------------------"

# 3. Publish ax-plat-aarch64-peripherals (depends on axplat, needs aarch64 target)
publish_crate "ax-plat-aarch64-peripherals" "platforms/axplat-aarch64-peripherals" "${AARCH64_TARGET}"

# 4. Publish aarch64 platform crates (all depend on ax-plat-aarch64-peripherals)
publish_crate "axplat-aarch64-qemu-virt" "platforms/axplat-aarch64-qemu-virt" "${AARCH64_TARGET}"
publish_crate "axplat-aarch64-raspi" "platforms/axplat-aarch64-raspi" "${AARCH64_TARGET}"
publish_crate "axplat-aarch64-bsta1000b" "platforms/axplat-aarch64-bsta1000b" "${AARCH64_TARGET}"
publish_crate "axplat-aarch64-phytium-pi" "platforms/axplat-aarch64-phytium-pi" "${AARCH64_TARGET}"

# 5. Publish x86_64 platform crate
publish_crate "ax-plat-x86-pc" "platforms/axplat-x86-pc" "${X86_64_TARGET}"

# 6. Publish riscv64 platform crate
publish_crate "ax-plat-riscv64-qemu-virt" "platforms/axplat-riscv64-qemu-virt" "${RISCV64_TARGET}"

# 7. Publish loongarch64 platform crate
publish_crate "ax-plat-loongarch64-qemu-virt" "platforms/axplat-loongarch64-qemu-virt" "${LOONGARCH64_TARGET}"

echo ""
echo "=========================================="
echo -e "${GREEN}All crates published successfully!${NC}"
echo "=========================================="
