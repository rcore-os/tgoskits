#!/bin/sh
# Comprehensive git-over-SSH carpet for StarryOS.
#
# Stands up a localhost sshd with key auth, then drives real git transport over
# ssh://: ls-remote, clone, push, fetch, pull, plus content assertions and
# negative controls (closed port, missing repo). Three-gate: PASS/FAIL/TOTAL
# vs EXPECTED, GIT_SSH_TEST_PASSED only when all pass.

EXPECTED=22

PASS=0
FAIL=0
TOTAL=0
SSHD_PID=""
WORK=/tmp/git-ssh-test
PORT=2222
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=3"

export GIT_SSH_COMMAND="ssh $SSH_OPTS"
export GIT_PAGER=cat
export PAGER=cat
export TERM=dumb
export GIT_AUTHOR_NAME="Carpet Tester"
export GIT_AUTHOR_EMAIL="carpet@starry.os"
export GIT_COMMITTER_NAME="Carpet Tester"
export GIT_COMMITTER_EMAIL="carpet@starry.os"
export GIT_AUTHOR_DATE="1700000000 +0000"
export GIT_COMMITTER_DATE="1700000000 +0000"
export LC_ALL=C

pass() { PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1)); }
fail() { FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1)); echo "FAIL[$TOTAL]: $1"; }

assert_eq() {
    if [ "$2" = "$3" ]; then pass; else fail "$1: expected=[$2] actual=[$3]"; fi
}
assert_contains() {
    case "$3" in *"$2"*) pass ;; *) fail "$1: [$2] not found in [$3]" ;; esac
}
assert_file() {
    if [ -f "$2" ]; then pass; else fail "$1: file missing: $2"; fi
}
assert_ok() {
    label=$1; shift
    if "$@" >/dev/null 2>&1; then pass; else fail "$label: failed: $*"; fi
}
assert_fails() {
    label=$1; shift
    if "$@" >/dev/null 2>&1; then fail "$label: unexpectedly succeeded: $*"; else pass; fi
}

cleanup() {
    if [ -n "$SSHD_PID" ]; then
        kill "$SSHD_PID" 2>/dev/null || true
        wait "$SSHD_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

hard_fail() {
    echo "$1"
    if [ -f "$WORK/sshd.log" ]; then
        echo "=== sshd log ==="
        cat "$WORK/sshd.log"
    fi
    echo "GIT_SSH_TEST_FAILED"
    exit 1
}

finish() {
    echo "=== git-ssh carpet summary: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED ==="
    if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ] && [ "$PASS" -eq "$EXPECTED" ]; then
        echo "GIT_SSH_TEST_PASSED"
        exit 0
    fi
    echo "GIT_SSH_TEST_FAILED"
    exit 1
}

install_packages() {
    if ! command -v git >/dev/null 2>&1 || ! command -v ssh >/dev/null 2>&1; then
        sed -i 's|https://|http://|' /etc/apk/repositories 2>/dev/null || true
        apk add git openssh >/dev/null 2>&1 || {
            apk update >/dev/null 2>&1
            apk add git openssh >/dev/null 2>&1
        }
    fi
    command -v git >/dev/null 2>&1 || return 1
    command -v ssh >/dev/null 2>&1 || return 1
    git --version
    ssh -V 2>&1
}

configure_ssh() {
    mkdir -p "$WORK" /root/.ssh /run /var/run
    chmod 700 /root/.ssh

    rm -f /etc/ssh/ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key.pub
    ssh-keygen -t ed25519 -f /etc/ssh/ssh_host_ed25519_key -N "" >/dev/null 2>&1

    rm -f /root/.ssh/id_ed25519 /root/.ssh/id_ed25519.pub
    ssh-keygen -t ed25519 -f /root/.ssh/id_ed25519 -N "" >/dev/null 2>&1
    cat /root/.ssh/id_ed25519.pub > /root/.ssh/authorized_keys
    chmod 600 /root/.ssh/authorized_keys

    sed -i 's/^#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config
    sed -i 's/^#PubkeyAuthentication.*/PubkeyAuthentication yes/' /etc/ssh/sshd_config

    /usr/sbin/sshd -D -e -p "$PORT" -o ListenAddress=127.0.0.1 >"$WORK/sshd.log" 2>&1 &
    SSHD_PID=$!
}

wait_for_ssh() {
    i=0
    while ! ssh $SSH_OPTS -p "$PORT" root@127.0.0.1 true >/dev/null 2>&1; do
        i=$((i + 1))
        [ "$i" -ge 20 ] && return 1
        sleep 1
    done
}

config_user() {
    git -C "$1" config user.email "carpet@starry.os"
    git -C "$1" config user.name "Carpet Tester"
}

prepare_repo() {
    rm -rf "$WORK/repo"
    mkdir -p "$WORK/repo"

    git init -q --bare "$WORK/repo/src.git"
    git -C "$WORK/repo/src.git" symbolic-ref HEAD refs/heads/main

    git init -q -b main "$WORK/repo/seed"
    config_user "$WORK/repo/seed"
    printf 'base\n' > "$WORK/repo/seed/data.txt"
    git -C "$WORK/repo/seed" add data.txt
    git -C "$WORK/repo/seed" commit -q -m "seed"
    git -C "$WORK/repo/seed" remote add origin "$WORK/repo/src.git"
    git -C "$WORK/repo/seed" push -q origin main
}

install_packages || hard_fail "git/openssh unavailable"
configure_ssh || hard_fail "sshd setup failed"
wait_for_ssh || hard_fail "sshd never became reachable"
prepare_repo || hard_fail "repo seed failed"

REMOTE="ssh://root@127.0.0.1:$PORT$WORK/repo/src.git"

# --- basic SSH reachability ---------------------------------------------------
assert_ok "ssh.reachable" ssh $SSH_OPTS -p "$PORT" root@127.0.0.1 true

# --- ls-remote over ssh:// ----------------------------------------------------
LSREMOTE=$(git ls-remote "$REMOTE" 2>/dev/null)
assert_contains "ssh.ls-remote.main" "refs/heads/main" "$LSREMOTE"
SEED_HEAD=$(git -C "$WORK/repo/seed" rev-parse HEAD)
assert_contains "ssh.ls-remote.sha" "$SEED_HEAD" "$LSREMOTE"
assert_contains "ssh.ls-remote.pattern" "refs/heads/main" "$(git ls-remote "$REMOTE" refs/heads/main 2>/dev/null)"

# --- clone over ssh:// --------------------------------------------------------
rm -rf "$WORK/repo/client" "$WORK/repo/puller"
assert_ok "ssh.clone" git clone -q "$REMOTE" "$WORK/repo/client"
assert_file "ssh.clone.file" "$WORK/repo/client/data.txt"
assert_eq "ssh.clone.content" "base" "$(cat "$WORK/repo/client/data.txt")"
assert_eq "ssh.clone.same-head" "$SEED_HEAD" "$(git -C "$WORK/repo/client" rev-parse HEAD)"

assert_ok "ssh.clone2" git clone -q "$REMOTE" "$WORK/repo/puller"
config_user "$WORK/repo/client"

# --- push over ssh:// ---------------------------------------------------------
printf 'from-client\n' >> "$WORK/repo/client/data.txt"
git -C "$WORK/repo/client" add data.txt
git -C "$WORK/repo/client" commit -q -m "client update"
CLIENT_HEAD=$(git -C "$WORK/repo/client" rev-parse HEAD)
assert_ok "ssh.push" git -C "$WORK/repo/client" push -q origin main
assert_eq "ssh.push.landed" "$CLIENT_HEAD" "$(git -C "$WORK/repo/src.git" rev-parse refs/heads/main)"

# --- fetch over ssh:// (no worktree change) -----------------------------------
assert_ok "ssh.fetch" git -C "$WORK/repo/puller" fetch -q origin main
assert_eq "ssh.fetch.remote-ref" "$CLIENT_HEAD" "$(git -C "$WORK/repo/puller" rev-parse FETCH_HEAD)"
assert_eq "ssh.fetch.worktree-unchanged" "base" "$(cat "$WORK/repo/puller/data.txt")"

# --- pull over ssh:// ---------------------------------------------------------
assert_ok "ssh.pull" git -C "$WORK/repo/puller" pull -q --ff-only origin main
assert_contains "ssh.pull.content" "from-client" "$(cat "$WORK/repo/puller/data.txt")"
assert_eq "ssh.pull.head" "$CLIENT_HEAD" "$(git -C "$WORK/repo/puller" rev-parse HEAD)"

# --- negative controls --------------------------------------------------------
assert_fails "ssh.closed-port" timeout 8 git ls-remote "ssh://root@127.0.0.1:3222$WORK/repo/src.git"
assert_fails "ssh.missing-repo" git ls-remote "ssh://root@127.0.0.1:$PORT$WORK/repo/missing.git"

finish
