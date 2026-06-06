#!/bin/sh
set -eu

# Create symlinks that can't be stored in the overlay.
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

# Sandbox test (nix.sh) is intentionally skipped — sandbox requires mount
# namespace isolation which is not yet fully available in StarryOS.
# Only run the nosandbox test.

echo "=== nix-nosandbox ==="
if /usr/bin/nix-nosandbox; then
    echo "NIX_NOSANDBOX_TEST_PASSED"
else
    echo "NIX_NOSANDBOX_TEST_FAILED"
fi
