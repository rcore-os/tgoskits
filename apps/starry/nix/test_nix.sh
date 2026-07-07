#!/bin/sh
set -eu

# Create symlinks that can't be stored in the overlay.
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

echo "=== nix-nosandbox ==="
if /usr/bin/nix-nosandbox; then
    echo "NIX_NOSANDBOX_TEST_PASSED"
else
    echo "NIX_NOSANDBOX_TEST_FAILED"
    exit 1
fi

echo "=== nix-nixpkgs ==="
if /usr/bin/nix-nixpkgs; then
    echo "NIX_NIXPKGS_TEST_PASSED"
else
    echo "NIX_NIXPKGS_TEST_FAILED"
    exit 1
fi

echo "NIX_NOSANDBOX_COMPLETE"
