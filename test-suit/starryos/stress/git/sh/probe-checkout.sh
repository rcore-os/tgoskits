#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/co-test; git init /tmp/git-test/co-test; cd /tmp/git-test/co-test
git config user.name "T"; git config user.email "t@t.com"
echo "f1" > f1.txt; git add f1.txt; git commit -m "first" >/dev/null 2>&1
DEFAULT_BRANCH=$(git branch --show-current)
git branch side
echo "f2" > f2.txt; git add f2.txt; git commit -m "second" >/dev/null 2>&1

section "1. checkout existing branch"; git checkout side 2>&1; [ "$(git branch --show-current)" = "side" ] && echo "PASS: git checkout existing" && PASS=$((PASS+1)) || { echo "FAIL: git checkout existing"; FAIL=$((FAIL+1)); }
section "2. checkout back to default"; git checkout "$DEFAULT_BRANCH" 2>/dev/null; [ "$(git branch --show-current)" = "$DEFAULT_BRANCH" ] && echo "PASS: git checkout default" && PASS=$((PASS+1)) || { echo "FAIL: git checkout default"; FAIL=$((FAIL+1)); }
section "3. checkout -b new branch"; git checkout -b newbr 2>/dev/null; [ "$(git branch --show-current)" = "newbr" ] && echo "PASS: git checkout -b" && PASS=$((PASS+1)) || { echo "FAIL: git checkout -b"; FAIL=$((FAIL+1)); }
section "4. switch to non-existent"; git checkout nonexistent 2>/dev/null && echo "FAIL: checkout nonexistent ok" && FAIL=$((FAIL+1)) || { echo "PASS: checkout nonexistent error"; PASS=$((PASS+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_CHECKOUT_ALL_PASSED" || echo "GIT_CHECKOUT_HAS_FAILURES"
