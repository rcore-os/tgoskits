#!/bin/sh
set -eu

# Create Nix subcommand symlinks (overlay doesn't support symlinks).
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

echo 'NIX_NIXPKGS_PHASE_PREBUILT_NIX_BEGIN'
command -v nix >/dev/null 2>&1 || fail 'prebuilt Nix is missing from case rootfs'
echo 'NIX_NIXPKGS_PHASE_PREBUILT_NIX_DONE'

echo 'NIX_NIXPKGS_PHASE_NIX_BEGIN'
nix --version || fail 'nix --version failed'
echo 'NIX_NIXPKGS_PHASE_NIX_DONE'

echo 'NIX_NIXPKGS_PHASE_BUILD_BEGIN'
rm -f ./result-nixpkgs

NIXPKGS_TARBALL="/nixpkgs.tar.gz"
NIXPKGS_DIR="/tmp/nixpkgs-src"

if [ ! -f "$NIXPKGS_TARBALL" ]; then
    fail "nixpkgs tarball not found at $NIXPKGS_TARBALL — prebuild should inject it"
fi

echo "NIX_NIXPKGS_INFO: extracting nixpkgs tarball..."
mkdir -p "$NIXPKGS_DIR"
# The tarball extracts to a single directory named nixpkgs-<rev>
tar xzf "$NIXPKGS_TARBALL" -C "$NIXPKGS_DIR"
NIXPKGS_SRC=$(echo "$NIXPKGS_DIR"/nixpkgs-*)
if [ ! -d "$NIXPKGS_SRC" ]; then
    fail "nixpkgs extraction failed — expected nixpkgs-* directory in $NIXPKGS_DIR"
fi
echo "NIX_NIXPKGS_INFO: nixpkgs extracted to $NIXPKGS_SRC"

cat > /tmp/nix-nixpkgs/default.nix <<NIXEOF
let
  pkgs = import $NIXPKGS_SRC {};
in
pkgs.stdenv.mkDerivation {
  name = "nixpkgs-hello";
  src = pkgs.writeText "hello.c" ''
    #include <stdio.h>
    int main(void) {
        printf("hello from nixpkgs stdenv\\n");
        return 0;
    }
  '';
  buildPhase = ''
    mkdir -p \$out/bin
    \$CC -o \$out/bin/hello \$src
  '';
  installPhase = "true";
  doCheck = false;
}
NIXEOF

echo 'NIX_NIXPKGS_INFO: nixpkgs build timeout is 600s'
echo "NIX_NIXPKGS_INFO: using nixpkgs from $NIXPKGS_SRC"

set +e
if command -v timeout >/dev/null 2>&1; then
    timeout 600 nix-build --option build-users-group '' --option sandbox false \
        /tmp/nix-nixpkgs/default.nix -o ./result-nixpkgs \
        >/tmp/nix-nixpkgs/build.log 2>&1
    build_rc=$?
else
    nix-build --option build-users-group '' --option sandbox false \
        /tmp/nix-nixpkgs/default.nix -o ./result-nixpkgs \
        >/tmp/nix-nixpkgs/build.log 2>&1
    build_rc=$?
fi
set -e

if [ "$build_rc" -ne 0 ]; then
    echo 'NIX_NIXPKGS_DIAG_BUILD_LOG_BEGIN'
    tail -80 /tmp/nix-nixpkgs/build.log
    echo 'NIX_NIXPKGS_DIAG_BUILD_LOG_END'
    echo 'NIX_NIXPKGS_DIAG_PS_BEGIN'
    ps
    echo 'NIX_NIXPKGS_DIAG_PS_END'
    echo "NIX_NIXPKGS_BUILD_EXIT=$build_rc"
    if grep -q 'interrupted by the user' /tmp/nix-nixpkgs/build.log; then
        fail 'nixpkgs build was interrupted after waiting for store lock'
    fi
    if grep -qi 'hash mismatch' /tmp/nix-nixpkgs/build.log; then
        fail 'nixpkgs tarball hash mismatch — update NIXPKGS_SHA256'
    fi
    fail 'nixpkgs stdenv.mkDerivation build failed'
fi

echo 'NIX_NIXPKGS_DIAG_BUILD_LOG_BEGIN'
tail -40 /tmp/nix-nixpkgs/build.log
echo 'NIX_NIXPKGS_DIAG_BUILD_LOG_END'

if [ ! -e ./result-nixpkgs/bin/hello ]; then
    echo 'NIX_NIXPKGS_DIAG_RESULT_BEGIN'
    ls -la ./result-nixpkgs/ 2>/dev/null || echo 'result-nixpkgs not found'
    ls -laR ./result-nixpkgs/bin/ 2>/dev/null || true
    echo 'NIX_NIXPKGS_DIAG_RESULT_END'
    fail 'nixpkgs result does not contain bin/hello'
fi

echo 'NIX_NIXPKGS_PHASE_VERIFY_BEGIN'
./result-nixpkgs/bin/hello | grep -q 'hello from nixpkgs stdenv' \
    || fail 'nixpkgs hello output mismatch'
echo 'NIX_NIXPKGS_PHASE_VERIFY_DONE'

echo 'NIX_NIXPKGS_PHASE_BUILD_DONE'
echo 'NIX_NIXPKGS_TEST_PASSED'
