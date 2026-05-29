#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/diff-test; mkdir -p /tmp/git-test/diff-test
git init /tmp/git-test/diff-test; cd /tmp/git-test/diff-test
git config user.name "T"; git config user.email "t@t.com"
echo "base" > file.txt; git add file.txt; git commit -m "base" >/dev/null 2>&1

section "1. diff working tree changes"; echo "modified" > file.txt
git diff 2>&1 | grep -q "modified" && echo "PASS: git diff working tree" && PASS=$((PASS+1)) || { echo "FAIL: git diff working tree"; FAIL=$((FAIL+1)); }
section "2. diff --cached"; git add file.txt
git diff --cached 2>&1 | grep -q "modified" && echo "PASS: git diff --cached" && PASS=$((PASS+1)) || { echo "FAIL: git diff --cached"; FAIL=$((FAIL+1)); }
section "3. diff no changes"; git commit -m "mod" >/dev/null 2>&1
git diff 2>/dev/null; [ $? -eq 0 ] && echo "PASS: git diff no changes" && PASS=$((PASS+1)) || { echo "FAIL: git diff no changes"; FAIL=$((FAIL+1)); }
section "4. diff between commits"; git diff HEAD~1 HEAD -- file.txt 2>&1 | grep -q "modified" && echo "PASS: git diff between commits" && PASS=$((PASS+1)) || { echo "FAIL: git diff between commits"; FAIL=$((FAIL+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_DIFF_ALL_PASSED" || echo "GIT_DIFF_HAS_FAILURES"
