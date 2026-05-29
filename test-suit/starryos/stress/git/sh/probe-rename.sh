#!/bin/sh
# Minimal rename reproduction for git branch -m bug
PASS=0; FAIL=0
section() { echo; echo "=== $1 ==="; }

BASE=/tmp/test-rename-bug
rm -rf "$BASE"
mkdir -p "$BASE"/refs/heads
echo "log content" > "$BASE"/refs/tmp-log

section "rename across sibling dirs (git reflog move)"
mv "$BASE"/refs/tmp-log "$BASE"/refs/heads/renamed-log 2>/tmp/rename-err.txt
rc=$?
if [ $rc -eq 0 ] && [ -f "$BASE"/refs/heads/renamed-log ]; then
    echo "PASS: rename across sibling dirs"; PASS=$((PASS+1))
else
    echo "FAIL: rename across sibling dirs (rc=$rc, err=$(cat /tmp/rename-err.txt))"; FAIL=$((FAIL+1))
fi

section "basic same-dir rename (sanity)"
echo "a" > "$BASE"/a.txt
mv "$BASE"/a.txt "$BASE"/b.txt 2>/tmp/rename-err2.txt
if [ $? -eq 0 ] && [ -f "$BASE"/b.txt ] && [ ! -f "$BASE"/a.txt ]; then
    echo "PASS: basic same-dir rename"; PASS=$((PASS+1))
else
    echo "FAIL: basic same-dir rename"; FAIL=$((FAIL+1))
fi

echo; echo "RESULT: PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "GIT_RENAME_ALL_PASSED" || echo "GIT_RENAME_HAS_FAILURES"
exit $((FAIL > 0 ? 1 : 0))
