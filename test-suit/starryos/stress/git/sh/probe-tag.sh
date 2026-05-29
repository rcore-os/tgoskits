#!/bin/sh
PASS=0; FAIL=0; section() { echo; echo "=== $1 ==="; }
rm -rf /tmp/git-test/tg-test; git init /tmp/git-test/tg-test; cd /tmp/git-test/tg-test
git config user.name "T"; git config user.email "t@t.com"
echo "init" > file.txt; git add file.txt; git commit -m "init" >/dev/null 2>&1

section "1. create lightweight tag"; git tag v1.0 2>/dev/null
git tag | grep -q "v1.0" && echo "PASS: git tag lightweight" && PASS=$((PASS+1)) || { echo "FAIL: git tag lightweight"; FAIL=$((FAIL+1)); }
section "2. create annotated tag"; git tag -a v2.0 -m "annotated" 2>/dev/null
git tag | grep -q "v2.0" && echo "PASS: git tag annotated" && PASS=$((PASS+1)) || { echo "FAIL: git tag annotated"; FAIL=$((FAIL+1)); }
section "3. list tags"; [ "$(git tag | wc -l)" -ge 2 ] && echo "PASS: git tag list" && PASS=$((PASS+1)) || { echo "FAIL: git tag list"; FAIL=$((FAIL+1)); }
section "4. delete tag"; git tag -d v1.0 2>/dev/null
! git tag | grep -q "v1.0" && echo "PASS: git tag delete" && PASS=$((PASS+1)) || { echo "FAIL: git tag delete"; FAIL=$((FAIL+1)); }
section "5. duplicate tag"; git tag v2.0 2>/dev/null && echo "FAIL: dup tag ok" && FAIL=$((FAIL+1)) || { echo "PASS: dup tag error"; PASS=$((PASS+1)); }

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_TAG_ALL_PASSED" || echo "GIT_TAG_HAS_FAILURES"
