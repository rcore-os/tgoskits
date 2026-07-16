#!/bin/sh
set -eu

export NIX_REMOTE=local

for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

fail() {
    echo "NIX_NIXPKGS_ERROR: $1"
    echo 'NIX_NIXPKGS_TEST_FAILED'
    exit 1
}

echo 'NIX_NIXPKGS_PHASE_ROOTFS_BEGIN'
mkdir -p /nix /etc/nix /tmp/nix-nixpkgs || fail 'failed to create nixpkgs test directories'

echo 'NIX_NIXPKGS_PHASE_NIX_BEGIN'
nix --version || fail 'nix --version failed'
echo 'NIX_NIXPKGS_PHASE_NIX_DONE'

echo 'NIX_NIXPKGS_PHASE_BUILD_BEGIN'
rm -f ./result-nixpkgs

nix-build /opt/nixpkgs -A hello -o ./result-nixpkgs \
    --option build-users-group '' --option sandbox false \
    > /tmp/nix-build.log 2>&1 || {
    echo 'NIX_NIXPKGS_BUILD_LOG_BEGIN'
    cat /tmp/nix-build.log
    echo 'NIX_NIXPKGS_BUILD_LOG_END'
    fail 'nixpkgs hello build failed'
}

echo 'NIX_NIXPKGS_PHASE_VERIFY_BEGIN'
./result-nixpkgs/bin/hello || fail 'nixpkgs hello execution failed'
echo 'NIX_NIXPKGS_PHASE_VERIFY_DONE'

echo 'NIX_NIXPKGS_PHASE_BUILD_DONE'
echo 'NIX_NIXPKGS_TEST_PASSED'
