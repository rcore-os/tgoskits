#!/bin/sh
# git remote probe: deterministic git:// URL served inside the guest.
PASS=0
FAIL=0
ROOT=/tmp/git-remote-test
PORT=9418
HOST=127.0.0.1
REMOTE="git://$HOST:$PORT/src.git"

section() {
    echo
    echo "=== $1 ==="
}

pass() {
    echo "PASS: $1"
    PASS=$((PASS + 1))
}

fail() {
    echo "FAIL: $1"
    FAIL=$((FAIL + 1))
}

cleanup() {
    if [ -n "$DAEMON_PID" ]; then
        kill "$DAEMON_PID" >/dev/null 2>&1 || true
        wait "$DAEMON_PID" >/dev/null 2>&1 || true
    fi
}

trap cleanup EXIT INT TERM

rm -rf "$ROOT"
mkdir -p "$ROOT"

section "prep bare repo"
git init --bare "$ROOT/src.git" >/dev/null 2>&1
git -C "$ROOT/src.git" config daemon.receivepack true
mkdir -p "$ROOT/work"
cd "$ROOT/work" || exit 1
git init >/dev/null 2>&1
git config user.name "T"
git config user.email "t@t.com"
git checkout -b main >/dev/null 2>&1
echo "hello from remote clone" > readme.txt
git add readme.txt
git commit -m "init" >/dev/null 2>&1
git remote add origin "$ROOT/src.git"
if git push -u origin main >/dev/null 2>"$ROOT/push.err"; then
    pass "prepared bare source repository"
else
    fail "prepare push failed: $(cat "$ROOT/push.err")"
fi
git -C "$ROOT/src.git" symbolic-ref HEAD refs/heads/main
cd "$ROOT" || exit 1

section "start git daemon"
git daemon \
    --reuseaddr \
    --export-all \
    --enable=receive-pack \
    --base-path="$ROOT" \
    --listen="$HOST" \
    --port="$PORT" \
    "$ROOT" >"$ROOT/git-daemon.log" 2>&1 &
DAEMON_PID=$!
sleep 1

if kill -0 "$DAEMON_PID" >/dev/null 2>&1; then
    pass "git daemon started"
else
    fail "git daemon failed to start: $(cat "$ROOT/git-daemon.log")"
fi

section "1. git ls-remote git://"
if git ls-remote "$REMOTE" refs/heads/main >"$ROOT/ls-remote.out" 2>"$ROOT/ls-remote.err" &&
    grep -q "refs/heads/main" "$ROOT/ls-remote.out"; then
    pass "git ls-remote over git://"
else
    fail "git ls-remote over git:// failed: $(cat "$ROOT/ls-remote.err")"
fi

section "2. git clone git://"
if git clone "$REMOTE" "$ROOT/clone-remote" >"$ROOT/clone.out" 2>"$ROOT/clone.err" &&
    [ -d "$ROOT/clone-remote/.git" ] &&
    grep -q "hello from remote clone" "$ROOT/clone-remote/readme.txt"; then
    pass "git clone remote URL"
else
    fail "git clone remote URL failed: $(cat "$ROOT/clone.err")"
fi

section "3. clone into existing empty dir from git://"
mkdir -p "$ROOT/existing-empty"
if git clone "$REMOTE" "$ROOT/existing-empty" >"$ROOT/clone-empty.out" 2>"$ROOT/clone-empty.err" &&
    grep -q "hello from remote clone" "$ROOT/existing-empty/readme.txt"; then
    pass "git clone remote URL into existing empty dir"
else
    fail "git clone remote URL into existing empty dir failed: $(cat "$ROOT/clone-empty.err")"
fi

section "4. clone from closed port should fail"
if git clone "git://$HOST:19418/src.git" "$ROOT/closed-port" >"$ROOT/closed-port.out" 2>"$ROOT/closed-port.err"; then
    fail "git clone from closed port unexpectedly succeeded"
else
    pass "git clone from closed port fails"
fi

section "5. git fetch from git://"
cd "$ROOT/work" || exit 1
echo "second commit" >> readme.txt
git add readme.txt
git commit -m "second" >/dev/null 2>&1
git push origin main >/dev/null 2>"$ROOT/push-second.err"
if [ $? -ne 0 ]; then
    fail "prepare second commit failed: $(cat "$ROOT/push-second.err")"
else
    if git -C "$ROOT/clone-remote" fetch origin main >"$ROOT/fetch.out" 2>"$ROOT/fetch.err" &&
        git -C "$ROOT/clone-remote" show --quiet --format=%s FETCH_HEAD | grep -q "second"; then
        pass "git fetch remote URL"
    else
        fail "git fetch remote URL failed: $(cat "$ROOT/fetch.err")"
    fi
fi

section "6. git pull from git://"
git clone "$REMOTE" "$ROOT/pull-client" >"$ROOT/pull-clone.out" 2>"$ROOT/pull-clone.err"
if [ $? -ne 0 ]; then
    fail "prepare pull client failed: $(cat "$ROOT/pull-clone.err")"
else
    cd "$ROOT/work" || exit 1
    echo "third commit" >> readme.txt
    git add readme.txt
    git commit -m "third" >/dev/null 2>&1
    git push origin main >/dev/null 2>"$ROOT/push-third.err"
    if [ $? -ne 0 ]; then
        fail "prepare third commit failed: $(cat "$ROOT/push-third.err")"
    elif git -C "$ROOT/pull-client" pull --ff-only origin main >"$ROOT/pull.out" 2>"$ROOT/pull.err" &&
        grep -q "third commit" "$ROOT/pull-client/readme.txt"; then
        pass "git pull remote URL"
    else
        fail "git pull remote URL failed: $(cat "$ROOT/pull.err")"
    fi
fi

section "7. git push to git://"
git clone "$REMOTE" "$ROOT/push-client" >"$ROOT/push-clone.out" 2>"$ROOT/push-clone.err"
if [ $? -ne 0 ]; then
    fail "prepare push client failed: $(cat "$ROOT/push-clone.err")"
else
    cd "$ROOT/push-client" || exit 1
    git config user.name "T"
    git config user.email "t@t.com"
    echo "client push" > pushed.txt
    git add pushed.txt
    git commit -m "client-push" >/dev/null 2>&1
    if git push origin main >"$ROOT/push-remote.out" 2>"$ROOT/push-remote.err" &&
        git --git-dir="$ROOT/src.git" show main:pushed.txt | grep -q "client push"; then
        pass "git push remote URL"
    else
        fail "git push remote URL failed: $(cat "$ROOT/push-remote.err")"
    fi
fi

echo
echo "RESULT: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_REMOTE_CLONE_ALL_PASSED"
    echo "GIT_REMOTE_FETCH_ALL_PASSED"
    echo "GIT_REMOTE_PULL_ALL_PASSED"
    echo "GIT_REMOTE_PUSH_ALL_PASSED"
    echo "GIT_REMOTE_ALL_PASSED"
else
    echo "GIT_REMOTE_HAS_FAILURES"
fi
exit $((FAIL > 0 ? 1 : 0))
