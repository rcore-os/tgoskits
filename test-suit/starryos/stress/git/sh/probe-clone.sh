#!/bin/sh
# git clone probe: local path, file:// URL, and error cases
PASS=0; FAIL=0
section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test
mkdir -p /tmp/git-test
cd /tmp/git-test

# ---- prep: a bare repo to clone from ----
rm -rf /tmp/git-test/src
git init --bare /tmp/git-test/src.git
mkdir -p /tmp/git-test/src-work
cd /tmp/git-test/src-work
git init; git config user.name "T"; git config user.email "t@t.com"
git checkout -b main 2>/dev/null  # ensure branch name consistency
echo "hello" > readme.txt
git add readme.txt; git commit -m "init" >/dev/null 2>&1
git remote add origin /tmp/git-test/src.git
git push -u origin main >/dev/null 2>&1
if [ $? -ne 0 ]; then
    echo "PREP FAIL: could not push to bare repo"
    exit 1
fi
git -C /tmp/git-test/src.git symbolic-ref HEAD refs/heads/main
cd /tmp/git-test

# 1. clone from local path
section "1. git clone <local-path>"
git clone /tmp/git-test/src.git clone-path 2>/tmp/git-test/clone-path.err
if [ -d clone-path/.git ] && [ -f clone-path/readme.txt ]; then
    echo "PASS: git clone local path"; PASS=$((PASS+1))
else
    echo "FAIL: git clone local path (err=$(cat /tmp/git-test/clone-path.err))"; FAIL=$((FAIL+1))
fi

# 2. clone from file:// URL
section "2. git clone file://"
git clone file:///tmp/git-test/src.git clone-url 2>/tmp/git-test/clone-url.err
if [ -d clone-url/.git ] && [ -f clone-url/readme.txt ]; then
    echo "PASS: git clone file://"; PASS=$((PASS+1))
else
    echo "FAIL: git clone file:// (err=$(cat /tmp/git-test/clone-url.err))"; FAIL=$((FAIL+1))
fi

# 3. clone into existing empty dir (should work)
section "3. clone into existing empty dir"
mkdir -p /tmp/git-test/existing-empty
git clone /tmp/git-test/src.git /tmp/git-test/existing-empty 2>/dev/null
if [ $? -eq 0 ] && [ -f /tmp/git-test/existing-empty/readme.txt ]; then
    echo "PASS: clone into existing empty dir ok"; PASS=$((PASS+1))
else
    echo "FAIL: clone into existing empty dir failed"; FAIL=$((FAIL+1))
fi

# 4. clone to non-empty dir (should fail)
section "4. clone into non-empty dir"
mkdir -p /tmp/git-test/nonempty
echo "junk" > /tmp/git-test/nonempty/file.txt
git clone /tmp/git-test/src.git /tmp/git-test/nonempty 2>/dev/null
if [ $? -ne 0 ]; then
    echo "PASS: clone into non-empty dir fails"; PASS=$((PASS+1))
else
    echo "FAIL: clone into non-empty dir succeeded"; FAIL=$((FAIL+1))
fi

# ---- summary ----
echo
echo "RESULT: PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_CLONE_ALL_PASSED"
else
    echo "GIT_CLONE_HAS_FAILURES"
fi
exit $((FAIL > 0 ? 1 : 0))
