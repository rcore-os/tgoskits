#!/bin/sh
set -eu

echo "=== nix-prebuilt ==="
if ! command -v nix >/dev/null 2>&1 || [ ! -x /usr/bin/nix ]; then
    echo "prebuilt Nix is missing from the app rootfs"
    echo "NIX_INSTALL_TEST_FAILED"
    exit 1
fi

nix --version
echo "NIX_INSTALL_TEST_PASSED"

echo "=== nix-nosandbox ==="
if /usr/bin/nix-nosandbox; then
    echo "NIX_NOSANDBOX_TEST_PASSED"
else
    echo "NIX_NOSANDBOX_TEST_FAILED"
    exit 1
fi

echo "=== nix-sandbox ==="
if /usr/bin/nix-sandbox; then
    echo "NIX_SANDBOX_TEST_PASSED"
else
    echo "NIX_SANDBOX_TEST_FAILED"
    exit 1
fi

echo "=== nix-nixpkgs ==="
if /usr/bin/nix-nixpkgs; then
    echo "NIX_NIXPKGS_TEST_PASSED"
else
    echo "NIX_NIXPKGS_TEST_FAILED"
    exit 1
fi

echo "NIX_APP_TESTS_PASSED"
