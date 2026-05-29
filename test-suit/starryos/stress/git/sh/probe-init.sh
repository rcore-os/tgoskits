#!/bin/sh
# git init probe: normal + edge + error cases
# inline test framework - no external deps
PASS=0; FAIL=0

section() { echo; echo "=== $1 ==="; }

# --- NORMAL ---
section "1. basic init"
rm -rf /tmp/git-test
mkdir -p /tmp/git-test
cd /tmp/git-test
git init basic
if [ -d basic/.git ] && [ -f basic/.git/HEAD ]; then
    echo "PASS: git init basic"; PASS=$((PASS+1))
else
    echo "FAIL: git init basic"; FAIL=$((FAIL+1))
fi

section "2. init with --initial-branch"
git init -b main branch-init
if [ -d branch-init/.git ]; then
    ref=$(cat branch-init/.git/HEAD 2>/dev/null)
    if echo "$ref" | grep -q "refs/heads/main"; then
        echo "PASS: git init --initial-branch=main"; PASS=$((PASS+1))
    else
        echo "FAIL: git init --initial-branch (HEAD=$ref)"; FAIL=$((FAIL+1))
    fi
else
    echo "FAIL: git init --initial-branch (.git missing)"; FAIL=$((FAIL+1))
fi

section "3. init in subdirectory"
rm -rf /tmp/git-test/deep
mkdir -p /tmp/git-test/deep/nested
git -C /tmp/git-test/deep/nested init sub-init
if [ -d /tmp/git-test/deep/nested/sub-init/.git ]; then
    echo "PASS: git init in nested subdirectory"; PASS=$((PASS+1))
else
    echo "FAIL: git init in nested subdirectory"; FAIL=$((FAIL+1))
fi

# --- EDGE ---
section "4. init existing directory (should work - reinit)"
cd /tmp/git-test
rm -rf reinit-test
mkdir reinit-test
echo "content" > reinit-test/file.txt
git init reinit-test
if [ -d reinit-test/.git ]; then
    # reinit should not destroy existing files
    if [ -f reinit-test/file.txt ]; then
        echo "PASS: git init in existing non-empty dir (reinit ok)"; PASS=$((PASS+1))
    else
        echo "FAIL: git init destroyed existing file"; FAIL=$((FAIL+1))
    fi
else
    echo "FAIL: git init in existing dir (reinit) - no .git"; FAIL=$((FAIL+1))
fi

section "5. init --bare"
rm -rf /tmp/git-test/bare-test
git init --bare /tmp/git-test/bare-test
if [ -f /tmp/git-test/bare-test/HEAD ] && [ -f /tmp/git-test/bare-test/config ]; then
    echo "PASS: git init --bare"; PASS=$((PASS+1))
else
    echo "FAIL: git init --bare"; FAIL=$((FAIL+1))
fi

section "6. init --template (empty template)"
rm -rf /tmp/git-test/tmpl
git init --template=/tmp/git-test/tmpl /tmp/git-test/template-test
if [ -d /tmp/git-test/template-test/.git ]; then
    echo "PASS: git init --template=<dir>"; PASS=$((PASS+1))
else
    echo "FAIL: git init --template=<dir>"; FAIL=$((FAIL+1))
fi

# --- ERROR ---
section "7. init on existing .git (should fail)"
git init basic 2>/dev/null
case $? in
    0) echo "INFO: git init on existing .git returned 0 (reinit allowed)" ;;
    *) echo "PASS: git init on existing .git returned error"; PASS=$((PASS+1)) ;;
esac

section "8. init with invalid option"
git init --nonexistent-flag /tmp/git-test/bogus 2>/dev/null
case $? in
    0) echo "FAIL: git init with invalid flag returned 0"; FAIL=$((FAIL+1)) ;;
    *) echo "PASS: git init with invalid flag returned error"; PASS=$((PASS+1)) ;;
esac

# --- SUMMARY ---
echo
echo "RESULT: PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_INIT_ALL_PASSED"
else
    echo "GIT_INIT_HAS_FAILURES"
fi
