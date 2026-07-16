#!/bin/sh
set -eu

export NIX_REMOTE=local

fail() {
    echo "NIX_SANDBOX_ERROR: $1"
    echo 'NIX_SANDBOX_TEST_FAILED'
    exit 1
}

dump_build() {
    sample=$1
    pid=$2
    state=$(awk '/^State:/{print $2}' "/proc/$pid/status" 2>/dev/null || echo '?')
    echo "NIX_SANDBOX_BUILD_SAMPLE sample=$sample pid=$pid state=$state"
}

build_is_running() {
    pid=$1
    kill -0 "$pid" 2>/dev/null || return 1
    [ -r "/proc/$pid/status" ] || return 1
    state=$(awk '/^State:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    [ -n "$state" ] && [ "$state" != 'Z' ]
}

run_build() {
    mode=$1
    expression=$2
    output=$3
    log=$4
    timeout=$5

    set +e
    nix-build -v --no-substitute --option build-users-group '' \
        --option sandbox "$mode" "$expression" \
        -o "$output" >"$log" 2>&1 &
    build_pid=$!
    set -e
    echo "NIX_SANDBOX_INFO: nix-build sandbox=$mode started pid=$build_pid"

    elapsed=0
    while build_is_running "$build_pid" && [ "$elapsed" -lt "$timeout" ]; do
        if [ $((elapsed % 15)) -eq 0 ]; then
            dump_build "$((elapsed / 15))" "$build_pid"
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    if build_is_running "$build_pid"; then
        echo "NIX_SANDBOX_INFO: ${timeout}s timeout, killing nix-build pid=$build_pid"
        kill "$build_pid" 2>/dev/null || true
        wait "$build_pid" 2>/dev/null || true
        return 124
    fi

    set +e
    wait "$build_pid"
    build_rc=$?
    set -e
    return "$build_rc"
}

echo 'NIX_SANDBOX_PHASE_INSTALL_BEGIN'
for cmd in build channel collect-garbage copy-closure env hash \
           instantiate prefetch-url shell store; do
    ln -sf nix "/usr/bin/nix-$cmd" 2>/dev/null || true
done
command -v nix >/dev/null 2>&1 || fail 'official Nix closure is missing from the app rootfs'
nix --version || fail 'nix --version failed'
echo 'NIX_SANDBOX_PHASE_INSTALL_DONE'

echo 'NIX_SANDBOX_PHASE_CONFIG_BEGIN'
mkdir -p /nix/var/nix /etc/nix /tmp/nix-sandbox
cat > /etc/nix/nix.conf <<'NIXCONF'
sandbox = true
build-users-group =
substituters = https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
NIXCONF
echo 'NIX_SANDBOX_PHASE_CONFIG_DONE'

echo 'NIX_SANDBOX_PHASE_DIAG_BEGIN'
echo "max_user_namespaces=$(cat /proc/sys/user/max_user_namespaces 2>&1)"
echo "kernel=$(cat /proc/version 2>&1 | head -1)"
echo 'NIX_SANDBOX_PHASE_DIAG_DONE'

echo 'NIX_SANDBOX_PHASE_BUILD_BEGIN'
rm -f ./result-nosandbox ./result-sandbox
cat > /tmp/nix-sandbox/sandbox.nix <<'NIXEOF'
derivation {
  name = "nix-sandbox";
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [
    "-c"
    "echo BUILDER_STARTED > /tmp/nix-sandbox/builder.log; echo OUT=\$out >> /tmp/nix-sandbox/builder.log; echo NIX_SANDBOX_BUILD_OK > \$out"
  ];
}
NIXEOF

if [ "$(uname -m)" = 'x86_64' ]; then
    echo 'NIX_SANDBOX_PHASE_BASELINE_BEGIN'
    sed 's/name = "nix-sandbox"/name = "nix-nosandbox"/' \
        /tmp/nix-sandbox/sandbox.nix > /tmp/nix-sandbox/nosandbox.nix
    if ! run_build false /tmp/nix-sandbox/nosandbox.nix \
        ./result-nosandbox /tmp/nix-sandbox/nosandbox.log 120; then
        cat /tmp/nix-sandbox/nosandbox.log 2>/dev/null || true
        fail 'non-sandboxed builder baseline failed'
    fi
    grep -q 'NIX_SANDBOX_BUILD_OK' ./result-nosandbox || fail 'non-sandboxed builder output marker missing'
    echo 'NIX_SANDBOX_PHASE_BASELINE_DONE'
fi

echo 'NIX_SANDBOX_INFO: sandboxed nix-build timeout is 45s'
trap 'echo "NIX_SANDBOX_TRAP: caught signal"' TERM HUP INT QUIT USR1 USR2
trap 'echo "NIX_SANDBOX_SCRIPT_EXIT: rc=$?"' EXIT

if run_build true /tmp/nix-sandbox/sandbox.nix \
    ./result-sandbox /tmp/nix-sandbox/build.log 45; then
    build_rc=0
else
    build_rc=$?
fi
echo "NIX_SANDBOX_BUILD_EXIT=$build_rc"

echo 'NIX_SANDBOX_BUILD_LOG_BEGIN'
cat /tmp/nix-sandbox/build.log 2>/dev/null || echo '(no build log)'
echo 'NIX_SANDBOX_BUILD_LOG_END'

if [ "$build_rc" -ne 0 ]; then
    echo 'NIX_SANDBOX_DIAG_FAILURE_BEGIN'
    dmesg 2>/dev/null | tail -30 || true
    cat /nix/var/nix/log/nix-daemon/*.log 2>/dev/null | tail -30 || true
    cat /tmp/nix-sandbox/builder.log 2>/dev/null || true
    echo 'NIX_SANDBOX_DIAG_FAILURE_END'
    fail "nix-build sandbox=true failed with exit $build_rc"
fi

echo 'NIX_SANDBOX_PHASE_VERIFY_BEGIN'
if grep -qi 'disabling sandbox\|sandbox.*disabled\|sandbox.*not supported' /tmp/nix-sandbox/build.log; then
    fail 'nix-build sandbox was disabled unexpectedly'
fi
[ -f ./result-sandbox ] || fail 'result-sandbox symlink not found'
cat ./result-sandbox || fail 'could not read result-sandbox'
grep -q 'NIX_SANDBOX_BUILD_OK' ./result-sandbox || fail 'sandbox build output marker missing'
echo 'NIX_SANDBOX_PHASE_VERIFY_DONE'

echo 'NIX_SANDBOX_BUILDER_LOG_BEGIN'
cat /tmp/nix-sandbox/builder.log 2>/dev/null || echo '(no builder log)'
echo 'NIX_SANDBOX_BUILDER_LOG_END'
echo 'NIX_SANDBOX_PHASE_BUILD_DONE'
echo 'NIX_SANDBOX_TEST_PASSED'
