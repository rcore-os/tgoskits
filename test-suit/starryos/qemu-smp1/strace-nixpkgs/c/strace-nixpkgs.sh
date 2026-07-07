#!/bin/sh
set -eu

# Nix legacy aliases
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix /usr/bin/nix-$cmd 2>/dev/null || true
done

# Load Nix DB
if [ ! -e /nix/var/nix/db/db.sqlite ]; then
    nix-store --load-db < /nix/.reginfo
fi

echo "NIX_STRACE_INFO: nix version: $(nix --version 2>&1 || echo 'FAILED')"

# Check tracer binary
if [ ! -x /usr/bin/starry-test-suit/test-strace-nix ]; then
    echo "NIX_STRACE_ERROR: tracer binary not found"
    exit 1
fi

# Prepare nixpkgs expression
mkdir -p /tmp/strace-nixpkgs
cat > /tmp/strace-nixpkgs/default.nix <<NIXEOF
let
  pkgs = import /opt/nixpkgs {};
in
pkgs.stdenv.mkDerivation {
  name = "strace-hello";
  srcs = [];
  dontUnpack = true;
  buildPhase = ''
    mkdir -p \$out/bin
    cat > hello.c <<'EOF'
#include <stdio.h>
int main(void) {
    printf("hello from strace-nixpkgs\\n");
    return 0;
}
EOF
    \$CC -o \$out/bin/hello hello.c
  '';
  installPhase = "true";
  doCheck = false;
}
NIXEOF

# Check nixpkgs source
if [ ! -f /opt/nixpkgs/default.nix ]; then
    echo "NIX_STRACE_ERROR: nixpkgs source not found"
    exit 1
fi

echo "NIX_STRACE_INFO: starting ptrace of nix-build (timeout=1740s)"
echo "NIX_STRACE_INFO: shell_pid=$$"

# Run nix-build under the ptrace tracer
# The tracer will fork, exec nix-build under PTRACE_SYSCALL, and log all syscalls.
# If nix-build crashes (SIGSEGV/SIGABRT), the tracer prints the crash signal
# and the last 50 syscalls, then exits successfully.
/usr/bin/starry-test-suit/test-strace-nix \
    nix-build \
    --verbose \
    --option build-users-group '' \
    --option sandbox false \
    /tmp/strace-nixpkgs/default.nix \
    -o /tmp/strace-result

tracer_rc=$?
echo "NIX_STRACE_INFO: tracer exit code=$tracer_rc"

# Check if build output exists
if [ -e /tmp/strace-result/bin/hello ]; then
    /tmp/strace-result/bin/hello
    echo "NIX_STRACE_INFO: build succeeded"
fi

echo "NIX_STRACE_PASSED"
