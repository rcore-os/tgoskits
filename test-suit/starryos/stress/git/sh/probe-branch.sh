#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/br-test; git init /tmp/git-test/br-test; cd /tmp/git-test/br-test
git config user.name "T"; git config user.email "t@t.com"
echo "init" > file.txt; git add file.txt; git commit -m "init" >/dev/null 2>&1

section "1. create branch"; git branch feat 2>/dev/null && [ "$(git branch --list feat | wc -l)" -gt 0 ] && echo "PASS: git branch create" && PASS=$((PASS+1)) || { echo "FAIL: git branch create"; FAIL=$((FAIL+1)); }
section "2. list branches"; git branch 2>&1 | grep -q "feat" && echo "PASS: git branch list" && PASS=$((PASS+1)) || { echo "FAIL: git branch list"; FAIL=$((FAIL+1)); }
section "3. delete branch"; git branch -d feat 2>/dev/null && ! git branch --list feat | grep -q "feat" && echo "PASS: git branch delete" && PASS=$((PASS+1)) || { echo "FAIL: git branch delete"; FAIL=$((FAIL+1)); }
section "4. rename branch"; git branch tmp; git branch -m tmp renamed; git branch --list renamed | grep -q "renamed" && echo "PASS: git branch rename" && PASS=$((PASS+1)) || { echo "FAIL: git branch rename"; FAIL=$((FAIL+1)); }
section "5. duplicate name"; git branch renamed 2>/dev/null && echo "FAIL: dup branch name ok" && FAIL=$((FAIL+1)) || { echo "PASS: dup branch name error"; PASS=$((PASS+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_BRANCH_ALL_PASSED" || echo "GIT_BRANCH_HAS_FAILURES"
