#!/bin/sh
set -eu

# Create the legacy Nix command aliases expected by the test scripts.
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

# Use builtins.toFile for store path creation (no builder subprocess).
# This verifies Nix expression evaluation + store write without depending on
# the builder communication protocol (socketpair), which requires poll
# notification semantics not yet complete on StarryOS.
# See nix.sh (sandbox variant) for the full derivation workflow once
# mount namespace isolation and builder protocol support are available.
eval_output="/tmp/nix-nosandbox/eval_output"
rm -f "$eval_output"
set +e
nix --extra-experimental-features nix-command eval --raw --expr \
  'builtins.toFile "nix-nosandbox" "NIX_LOCAL_BUILD_NOSANDBOX_OK"' \
  >"$eval_output" 2>/tmp/nix-nosandbox/eval.log
eval_rc=$?
set -e
if [ "$eval_rc" -ne 0 ]; then
    echo 'NIX_NOSANDBOX_DIAG_EVAL_LOG_BEGIN'
    cat /tmp/nix-nosandbox/eval.log
    echo 'NIX_NOSANDBOX_DIAG_EVAL_LOG_END'
    echo "NIX_NOSANDBOX_EVAL_EXIT=$eval_rc"
    fail 'tiny local nix eval failed'
fi
store_path=$(cat "$eval_output")
store_path=$(echo "$store_path" | tr -d '\n\r')  # strip trailing newlines

echo 'NIX_NOSANDBOX_DIAG_STORE_PATH_BEGIN'
echo "$store_path"
echo 'NIX_NOSANDBOX_DIAG_STORE_PATH_END'

if [ ! -f "$store_path" ]; then
    fail "store path does not exist: $store_path"
fi

cat "$store_path" || fail 'nix store output could not be read'
grep -q 'NIX_LOCAL_BUILD_NOSANDBOX_OK' "$store_path" || fail 'nix store output marker missing'
echo 'NIX_NOSANDBOX_PHASE_BUILD_DONE'
echo 'NIX_NOSANDBOX_TEST_PASSED'
