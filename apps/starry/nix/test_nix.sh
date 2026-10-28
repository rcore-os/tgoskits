#!/bin/sh
set -eu

skip_sandbox=0
if [ "$#" -eq 1 ] && [ "$1" = "--ci" ]; then
    skip_sandbox=1
elif [ "$#" -ne 0 ]; then
    echo "usage: $0 [--ci]" >&2
    exit 2
fi
passed=0
skipped=0

echo "=== nix-prebuilt ==="
if ! command -v nix >/dev/null 2>&1 || [ ! -x /usr/bin/nix ]; then
    echo "prebuilt Nix is missing from the app rootfs"
    echo "NIX_INSTALL_TEST_FAILED"
    exit 1
fi

nix --version
echo "NIX_INSTALL_TEST_PASSED"
passed=$((passed + 1))

echo "=== nix-nosandbox ==="
if /usr/bin/nix-nosandbox; then
    echo "NIX_NOSANDBOX_TEST_PASSED"
    passed=$((passed + 1))
else
    echo "NIX_NOSANDBOX_TEST_FAILED"
    exit 1
fi

echo "=== nix-sandbox ==="
if [ "$skip_sandbox" = 1 ]; then
    echo "NIX_SANDBOX_TEST_SKIPPED reason=nonidentity_uid_gid_mapping_gap"
    skipped=$((skipped + 1))
elif /usr/bin/nix-sandbox; then
    echo "NIX_SANDBOX_TEST_PASSED"
    passed=$((passed + 1))
else
    echo "NIX_SANDBOX_TEST_FAILED"
    exit 1
fi

echo "=== nix-nixpkgs ==="
if /usr/bin/nix-nixpkgs; then
    echo "NIX_NIXPKGS_TEST_PASSED"
    passed=$((passed + 1))
else
    echo "NIX_NIXPKGS_TEST_FAILED"
    exit 1
fi

echo "NIX_APP_TESTS_SUMMARY passed=$passed skipped=$skipped total=$((passed + skipped))"
echo "NIX_APP_TESTS_PASSED"
