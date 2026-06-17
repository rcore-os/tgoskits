#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/log-test; mkdir -p /tmp/git-test/log-test
git init /tmp/git-test/log-test; cd /tmp/git-test/log-test
git config user.name "T"; git config user.email "t@t.com"
for i in 1 2 3; do echo "c$i" > f$i.txt; git add f$i.txt; git commit -m "commit $i" >/dev/null 2>&1; done

section "1. basic log"; git log --oneline 2>&1 | grep -q "commit 1" && echo "PASS: git log" && PASS=$((PASS+1)) || { echo "FAIL: git log"; FAIL=$((FAIL+1)); }
section "2. log --oneline"; [ $(git log --oneline | wc -l) -eq 3 ] && echo "PASS: git log --oneline count" && PASS=$((PASS+1)) || { echo "FAIL: git log --oneline count"; FAIL=$((FAIL+1)); }
section "3. log -1"; git log -1 --format="%s" | grep -q "commit 3" && echo "PASS: git log -1" && PASS=$((PASS+1)) || { echo "FAIL: git log -1"; FAIL=$((FAIL+1)); }
section "4. log --stat"; git log --stat -1 2>&1 | grep -q "f3.txt" && echo "PASS: git log --stat" && PASS=$((PASS+1)) || { echo "FAIL: git log --stat"; FAIL=$((FAIL+1)); }
section "5. log with path filter"; git log --oneline -- f2.txt 2>&1 | grep -q "commit 2" && echo "PASS: git log path filter" && PASS=$((PASS+1)) || { echo "FAIL: git log path filter"; FAIL=$((FAIL+1)); }
section "6. log empty repo"; rm -rf /tmp/git-test/log-empty; git init /tmp/git-test/log-empty
git -C /tmp/git-test/log-empty log 2>&1 | grep -q "does not have any commits" && echo "PASS: git log empty repo" && PASS=$((PASS+1)) || { echo "INFO: git log empty repo ok"; PASS=$((PASS+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_LOG_ALL_PASSED" || echo "GIT_LOG_HAS_FAILURES"
exit $((FAIL > 0 ? 1 : 0))
