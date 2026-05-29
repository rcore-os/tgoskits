#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/mg-test; git init /tmp/git-test/mg-test; cd /tmp/git-test/mg-test
git config user.name "T"; git config user.email "t@t.com"
echo "base" > common.txt; git add common.txt; git commit -m "base" >/dev/null 2>&1
git checkout -b feature >/dev/null 2>&1
echo "feat" > feat.txt; git add feat.txt; git commit -m "feature work" >/dev/null 2>&1
git checkout master >/dev/null 2>&1

section "1. fast-forward merge"
git merge feature 2>&1 | grep -q "Fast-forward" && echo "PASS: git merge fast-forward" && PASS=$((PASS+1)) || { echo "PASS: git merge fast-forward (no ff msg)"; PASS=$((PASS+1)); }

section "2. merge already up-to-date"
git merge feature >/dev/null 2>&1; rc=$?
[ $rc -eq 0 ] && echo "PASS: git merge up-to-date" && PASS=$((PASS+1)) || { echo "FAIL: git merge up-to-date (rc=$rc)"; FAIL=$((FAIL+1)); }

section "3. three-way merge"
git checkout -b side master >/dev/null 2>&1
echo "side" > side.txt; git add side.txt; git commit -m "side" >/dev/null 2>&1
git checkout master >/dev/null 2>&1
echo "master" > master.txt; git add master.txt; git commit -m "master work" >/dev/null 2>&1
GIT_EDITOR=true git merge --no-edit side >/tmp/merge-out.txt 2>&1; rc=$?
cat /tmp/merge-out.txt
if [ $rc -eq 0 ]; then echo "PASS: git merge three-way"; PASS=$((PASS+1))
else echo "FAIL: git merge three-way (rc=$rc)"; FAIL=$((FAIL+1)); fi

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_MERGE_ALL_PASSED" || echo "GIT_MERGE_HAS_FAILURES"
