#!/bin/sh
set -eu

SSHD_PID=""
WORK=/tmp/git-ssh-test
PORT=2222
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=3"
export GIT_SSH_COMMAND="ssh $SSH_OPTS"
export GIT_PAGER=cat
export PAGER=cat
export TERM=dumb

cleanup() {
    if [ -n "$SSHD_PID" ]; then
        kill "$SSHD_PID" 2>/dev/null || true
        wait "$SSHD_PID" 2>/dev/null || true
    fi
}

fail() {
    echo "GIT_SSH_TEST_FAILED"
    if [ -f "$WORK/sshd.log" ]; then
        echo "=== sshd log ==="
        cat "$WORK/sshd.log"
    fi
    exit 1
}

trap cleanup EXIT

install_packages() {
    sed -i 's|https://|http://|' /etc/apk/repositories
    apk add git openssh || {
        apk update
        apk add git openssh
    }

    git --version
    ssh -V 2>&1
}

configure_ssh() {
    mkdir -p "$WORK" /root/.ssh /run /var/run
    chmod 700 /root/.ssh

    rm -f /etc/ssh/ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key.pub
    ssh-keygen -t ed25519 -f /etc/ssh/ssh_host_ed25519_key -N ""

    rm -f /root/.ssh/id_ed25519 /root/.ssh/id_ed25519.pub
    ssh-keygen -t ed25519 -f /root/.ssh/id_ed25519 -N ""
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
        if [ "$i" -ge 20 ]; then
            return 1
        fi
        sleep 1
    done
}

config_user() {
    repo=$1
    git -C "$repo" config user.email "test@starry.os"
    git -C "$repo" config user.name "Starry Git SSH"
}

prepare_repo() {
    rm -rf "$WORK/repo"
    mkdir -p "$WORK/repo"

    git init --bare "$WORK/repo/src.git"
    git -C "$WORK/repo/src.git" symbolic-ref HEAD refs/heads/main

    git init -b main "$WORK/repo/seed"
    config_user "$WORK/repo/seed"
    printf 'base\n' > "$WORK/repo/seed/data.txt"
    git -C "$WORK/repo/seed" add data.txt
    git -C "$WORK/repo/seed" commit -m "seed"
    git -C "$WORK/repo/seed" remote add origin "$WORK/repo/src.git"
    git -C "$WORK/repo/seed" push origin main
}

run_git_ssh_remote() {
    remote="ssh://root@127.0.0.1:$PORT$WORK/repo/src.git"

    git ls-remote "$remote" refs/heads/main | grep refs/heads/main

    git clone "$remote" "$WORK/repo/client"
    git clone "$remote" "$WORK/repo/puller"

    config_user "$WORK/repo/client"
    printf 'from-client\n' >> "$WORK/repo/client/data.txt"
    git -C "$WORK/repo/client" add data.txt
    git -C "$WORK/repo/client" commit -m "client update"
    git -C "$WORK/repo/client" push origin main

    git -C "$WORK/repo/puller" fetch origin main
    git -C "$WORK/repo/puller" pull --ff-only origin main
    grep from-client "$WORK/repo/puller/data.txt"

    if timeout 5 git ls-remote "ssh://root@127.0.0.1:3222$WORK/repo/src.git" >/tmp/git-ssh-closed-port.out 2>&1; then
        echo "unexpected success for closed ssh port"
        return 1
    fi

    if git ls-remote "ssh://root@127.0.0.1:$PORT$WORK/repo/missing.git" >/tmp/git-ssh-missing.out 2>&1; then
        echo "unexpected success for missing ssh repo"
        return 1
    fi

    echo "GIT_SSH_REMOTE_PASSED"
}

install_packages || fail
configure_ssh || fail
wait_for_ssh || fail
prepare_repo || fail
run_git_ssh_remote || fail

echo "GIT_SSH_TEST_PASSED"
