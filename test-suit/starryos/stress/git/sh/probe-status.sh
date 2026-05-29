#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/status-test; mkdir -p /tmp/git-test/status-test
git init /tmp/git-test/status-test; cd /tmp/git-test/status-test
git config user.name "T"; git config user.email "t@t.com"

section "1. status clean repo"; git status 2>&1 | grep -q "nothing to commit" && echo "PASS: git status clean" && PASS=$((PASS+1)) || { echo "FAIL: git status clean"; FAIL=$((FAIL+1)); }
section "2. status untracked file"; echo "new" > untracked.txt; git status 2>&1 | grep -q "untracked" && echo "PASS: git status untracked" && PASS=$((PASS+1)) || { echo "FAIL: git status untracked"; FAIL=$((FAIL+1)); }
section "3. status staged file"; git add untracked.txt; git status 2>&1 | grep -q "new file" && echo "PASS: git status staged" && PASS=$((PASS+1)) || { echo "FAIL: git status staged"; FAIL=$((FAIL+1)); }
section "4. status after commit"; git commit -m "add" >/dev/null 2>&1; git status 2>&1 | grep -q "nothing to commit" && echo "PASS: git status after commit" && PASS=$((PASS+1)) || { echo "FAIL: git status after commit"; FAIL=$((FAIL+1)); }
section "5. status modified"; echo "changed" > untracked.txt; git status 2>&1 | grep -q "modified" && echo "PASS: git status modified" && PASS=$((PASS+1)) || { echo "FAIL: git status modified"; FAIL=$((FAIL+1)); }
section "6. status deleted"; rm untracked.txt; git status 2>&1 | grep -q "deleted" && echo "PASS: git status deleted" && PASS=$((PASS+1)) || { echo "FAIL: git status deleted"; FAIL=$((FAIL+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_STATUS_ALL_PASSED" || echo "GIT_STATUS_HAS_FAILURES"
