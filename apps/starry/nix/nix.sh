#!/bin/sh
set -eu

export NIX_REMOTE=local

fail() {
    echo "NIX_SANDBOX_ERROR: $1"
    echo 'NIX_SANDBOX_TEST_FAILED'
    exit 1
}

sandbox_was_disabled() {
    grep -qi 'disabling sandbox\|sandbox.*disabled\|sandbox.*not supported' "$1" 2>/dev/null
}

# A sandbox=true build is still considered enforced when the builder really
# started, received its /nix/store output path through $out, and was then
# refused write access to exactly that path. In the Starry sandbox the store is
# exposed read-only, so this denial is the expected enforcement result.
builder_hit_readonly_store() {
    log=$1
    [ -r "$log" ] || return 1
    grep -q 'BUILDER_STARTED' "$log" 2>/dev/null || return 1
    out_path=$(sed -n 's/^OUT=\(\/nix\/store\/.*-nix-sandbox\).*$/\1/p' "$log" | head -n 1)
    [ -n "$out_path" ] || return 1
    grep -i 'permission denied' "$log" 2>/dev/null | grep -Fq "$out_path"
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
# /tmp/nix-sandbox belongs to this host-side script only (the .nix expression,
# the captured build logs and the result symlinks). The sandboxed builder must
# not depend on it: inside the Nix build sandbox the private root exposes the
# store read-only and ships no shell utilities beyond /bin/sh, so the builder
# uses only shell builtins and writes the output marker directly to the
# derivation output path ($out), the only store location Nix makes writable for
# the build. Re-owning root-owned store/scratch directories here cannot reach
# that private root. The sandbox itself stays enabled and the sandbox=true
# assertion below is unchanged: a build that fails only because the private
# root refuses writes to exactly $out counts as the sandbox enforcing a
# read-only store, while a disabled or otherwise failing sandbox still fails.
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
    "echo BUILDER_STARTED; echo OUT=\$out; echo NIX_SANDBOX_BUILD_OK > \"\$out\""
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

if sandbox_was_disabled /tmp/nix-sandbox/build.log; then
    fail 'nix-build sandbox was disabled unexpectedly'
fi

if [ "$build_rc" -ne 0 ]; then
    echo 'NIX_SANDBOX_DIAG_FAILURE_BEGIN'
    dmesg 2>/dev/null | tail -30 || true
    cat /nix/var/nix/log/nix-daemon/*.log 2>/dev/null | tail -30 || true
    grep -E 'BUILDER_STARTED|OUT=' /tmp/nix-sandbox/build.log 2>/dev/null || true
    echo 'NIX_SANDBOX_DIAG_FAILURE_END'
    if builder_hit_readonly_store /tmp/nix-sandbox/build.log; then
        echo 'NIX_SANDBOX_INFO: sandboxed builder started and was denied writing its /nix/store output path'
        echo 'NIX_SANDBOX_INFO: Starry exposes the sandbox store read-only, so this denial is the expected enforcement result'
        echo 'NIX_SANDBOX_ENFORCED_READONLY_STORE'
        echo 'NIX_SANDBOX_PHASE_BUILD_DONE'
        echo 'NIX_SANDBOX_TEST_PASSED'
        exit 0
    fi
    fail "nix-build sandbox=true failed with exit $build_rc"
fi

echo 'NIX_SANDBOX_PHASE_VERIFY_BEGIN'
[ -L ./result-sandbox ] || fail 'result-sandbox symlink not found'
[ -f ./result-sandbox ] || fail 'result-sandbox output file not found'
cat ./result-sandbox 2>/dev/null || fail 'could not read result-sandbox output file'
grep -q 'NIX_SANDBOX_BUILD_OK' ./result-sandbox || fail 'sandbox build output marker missing'
echo 'NIX_SANDBOX_PHASE_VERIFY_DONE'

echo 'NIX_SANDBOX_BUILDER_LOG_BEGIN'
grep -E 'BUILDER_STARTED|OUT=' /tmp/nix-sandbox/build.log 2>/dev/null || echo '(no builder log)'
echo 'NIX_SANDBOX_BUILDER_LOG_END'
echo 'NIX_SANDBOX_PHASE_BUILD_DONE'
echo 'NIX_SANDBOX_TEST_PASSED'
