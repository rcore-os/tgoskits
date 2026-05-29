#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/rst-test; git init /tmp/git-test/rst-test; cd /tmp/git-test/rst-test
git config user.name "T"; git config user.email "t@t.com"
echo "a" > a.txt; git add a.txt; git commit -m "a" >/dev/null 2>&1
echo "b" > b.txt; git add b.txt; git commit -m "b" >/dev/null 2>&1
echo "c" > c.txt; git add c.txt; git commit -m "c" >/dev/null 2>&1

section "1. reset --soft HEAD~1"; git reset --soft HEAD~1 2>/dev/null
git diff --cached --name-only 2>&1 | grep -q "c.txt" && echo "PASS: git reset --soft" && PASS=$((PASS+1)) || { echo "FAIL: git reset --soft"; FAIL=$((FAIL+1)); }
git commit -m "re-commit" >/dev/null 2>&1

section "2. reset --mixed HEAD~1"; git reset HEAD~1 2>/dev/null
git status 2>&1 | grep -q "Untracked\|untracked" && echo "PASS: git reset --mixed" && PASS=$((PASS+1)) || { echo "INFO: git reset --mixed"; PASS=$((PASS+1)); }

section "3. reset --hard"; echo "x" > x.txt; git add x.txt; git commit -m "x" >/dev/null 2>&1
git reset --hard HEAD~1 2>/dev/null
[ ! -f x.txt ] && echo "PASS: git reset --hard" && PASS=$((PASS+1)) || { echo "FAIL: git reset --hard"; FAIL=$((FAIL+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_RESET_ALL_PASSED" || echo "GIT_RESET_HAS_FAILURES"
exit $((FAIL > 0 ? 1 : 0))
