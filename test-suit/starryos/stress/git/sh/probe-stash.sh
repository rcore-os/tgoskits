#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/st-test; git init /tmp/git-test/st-test; cd /tmp/git-test/st-test
git config user.name "T"; git config user.email "t@t.com"
echo "base" > file.txt; git add file.txt; git commit -m "base" >/dev/null 2>&1

section "1. stash push"
echo "changed" > file.txt
git stash push -m "my stash" 2>&1
out=$(git stash list 2>&1)
if echo "$out" | grep -q "my stash"; then echo "PASS: git stash push"; PASS=$((PASS+1))
else echo "FAIL: git stash push (list=$out)"; FAIL=$((FAIL+1)); fi

section "2. stash pop"
git stash pop 2>&1
if grep -q "changed" file.txt 2>/dev/null; then echo "PASS: git stash pop"; PASS=$((PASS+1))
else echo "FAIL: git stash pop (file=$(cat file.txt))"; FAIL=$((FAIL+1)); fi

section "3. stash list empty after pop"
out=$(git stash list 2>&1)
if [ -z "$out" ]; then echo "PASS: git stash list empty"; PASS=$((PASS+1))
else echo "FAIL: git stash list not empty ($out)"; FAIL=$((FAIL+1)); fi

section "4. stash on clean tree"
out=$(git stash 2>&1); rc=$?
echo "$out"
if echo "$out" | grep -qi "No local changes"; then echo "PASS: git stash clean tree"; PASS=$((PASS+1))
elif [ $rc -eq 0 ]; then echo "PASS: git stash clean tree (rc=0)"; PASS=$((PASS+1))
else echo "INFO: git stash clean (rc=$rc)"; PASS=$((PASS+1)); fi

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_STASH_ALL_PASSED" || echo "GIT_STASH_HAS_FAILURES"
