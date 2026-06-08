#!/bin/sh
set -eu

# Create Nix subcommand symlinks (overlay doesn't support symlinks).
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

fail() {
    echo "NIX_NOSANDBOX_ERROR: $1"
    echo 'NIX_NOSANDBOX_TEST_FAILED'
    exit 1
}

echo 'NIX_NOSANDBOX_PHASE_ROOTFS_BEGIN'
mkdir -p /nix /etc/nix /tmp/nix-nosandbox || fail 'failed to create Nix smoke directories'

echo 'NIX_NOSANDBOX_PHASE_PREBUILT_NIX_BEGIN'
command -v nix >/dev/null 2>&1 || fail 'prebuilt Nix is missing from case rootfs'
echo 'NIX_NOSANDBOX_PHASE_PREBUILT_NIX_DONE'

echo 'NIX_NOSANDBOX_PHASE_NIX_BEGIN'
nix --version || fail 'nix --version failed'
echo 'NIX_NOSANDBOX_PHASE_NIX_DONE'

echo 'NIX_NOSANDBOX_PHASE_BUILD_BEGIN'
rm -f ./result
cat > /tmp/nix-nosandbox/default.nix <<'EOF'
derivation {
  name = "nix-nosandbox";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "/bin/mkdir -p /tmp/nix-nosandbox && echo BUILDER_STARTED > /tmp/nix-nosandbox/builder.log && echo OUT=$out >> /tmp/nix-nosandbox/builder.log && echo NIX_LOCAL_BUILD_NOSANDBOX_OK > $out" ];
}
EOF
echo 'NIX_NOSANDBOX_INFO: tiny local unsandboxed nix-build timeout is 120s'
set +e
if command -v timeout >/dev/null 2>&1; then
    timeout 120 nix-build --no-substitute --option build-users-group '' --option sandbox false /tmp/nix-nosandbox/default.nix >/tmp/nix-nosandbox/build.log 2>&1
    build_rc=$?
else
    nix-build --no-substitute --option build-users-group '' --option sandbox false /tmp/nix-nosandbox/default.nix >/tmp/nix-nosandbox/build.log 2>&1
    build_rc=$?
fi
set -e
if [ "$build_rc" -ne 0 ]; then
    echo 'NIX_NOSANDBOX_DIAG_BUILDER_LOG_BEGIN'
    cat /tmp/nix-nosandbox/builder.log 2>/dev/null || echo 'NIX_DIAG_NO_BUILDER_LOG'
    echo 'NIX_NOSANDBOX_DIAG_BUILDER_LOG_END'
    echo 'NIX_NOSANDBOX_DIAG_BUILD_LOG_BEGIN'
    cat /tmp/nix-nosandbox/build.log
    echo 'NIX_NOSANDBOX_DIAG_BUILD_LOG_END'
    echo 'NIX_NOSANDBOX_DIAG_PS_BEGIN'
    ps
    echo 'NIX_NOSANDBOX_DIAG_PS_END'
    echo 'NIX_NOSANDBOX_DIAG_FIND_BEGIN'
    find /nix/store -maxdepth 1 -name '*-nix' -exec ls -la {} \;
    echo 'NIX_NOSANDBOX_DIAG_FIND_END'
    echo "NIX_NOSANDBOX_BUILD_EXIT=$build_rc"
    if grep -q 'interrupted by the user' /tmp/nix-nosandbox/build.log; then
        fail 'tiny local unsandboxed nix-build was interrupted after waiting for store lock'
    fi
    fail 'tiny local unsandboxed nix-build failed'
fi
cat /tmp/nix-nosandbox/build.log
cat ./result || fail 'nix-build result symlink could not be read'
grep -q 'NIX_LOCAL_BUILD_NOSANDBOX_OK' ./result || fail 'tiny local unsandboxed nix-build output marker missing'
echo 'NIX_NOSANDBOX_PHASE_BUILD_DONE'
echo 'NIX_NOSANDBOX_TEST_PASSED'
