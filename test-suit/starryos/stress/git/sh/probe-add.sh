#!/bin/sh
# git add probe: normal + edge + error cases
PASS=0; FAIL=0
section() { echo; echo "=== $1 ==="; }

rm -rf /tmp/git-test/add-test
mkdir -p /tmp/git-test/add-test
git init /tmp/git-test/add-test
cd /tmp/git-test/add-test

# --- NORMAL ---
section "1. add single file"
echo "hello" > file1.txt
git add file1.txt
if git ls-files --stage | grep -q "file1.txt"; then
    echo "PASS: git add single file"; PASS=$((PASS+1))
else
    echo "FAIL: git add single file"; FAIL=$((FAIL+1))
fi

section "2. add multiple files"
echo "world" > file2.txt
echo "!" > file3.txt
git add file2.txt file3.txt
if git ls-files --stage | grep -q "file2.txt" && git ls-files --stage | grep -q "file3.txt"; then
    echo "PASS: git add multiple files"; PASS=$((PASS+1))
else
    echo "FAIL: git add multiple files"; FAIL=$((FAIL+1))
fi

section "3. add directory"
mkdir subdir
echo "subfile" > subdir/file.txt
git add subdir
if git ls-files --stage | grep -q "subdir/file.txt"; then
    echo "PASS: git add directory"; PASS=$((PASS+1))
else
    echo "FAIL: git add directory"; FAIL=$((FAIL+1))
fi

# --- EDGE ---
section "4. add empty file"
touch empty.txt
git add empty.txt
if git ls-files --stage | grep -q "empty.txt"; then
    echo "PASS: git add empty file"; PASS=$((PASS+1))
else
    echo "FAIL: git add empty file"; FAIL=$((FAIL+1))
fi

section "5. add file with spaces in name"
echo "spaces" > "file with spaces.txt"
git add "file with spaces.txt"
if git ls-files --stage | grep -q "file with spaces.txt"; then
    echo "PASS: git add file with spaces in name"; PASS=$((PASS+1))
else
    echo "FAIL: git add file with spaces in name"; FAIL=$((FAIL+1))
fi

section "6. add via glob"
echo "glob1" > glob-a.txt
echo "glob2" > glob-b.txt
git add glob-*.txt
if git ls-files --stage | grep -q "glob-a.txt" && git ls-files --stage | grep -q "glob-b.txt"; then
    echo "PASS: git add via glob"; PASS=$((PASS+1))
else
    echo "FAIL: git add via glob"; FAIL=$((FAIL+1))
fi

section "7. add updated file (already staged)"
echo "updated" >> file1.txt
git add file1.txt
if git ls-files --stage | grep -q "file1.txt"; then
    echo "PASS: git add updated staged file"; PASS=$((PASS+1))
else
    echo "FAIL: git add updated staged file"; FAIL=$((FAIL+1))
fi

# --- ERROR ---
section "8. add non-existent file"
if git add nonexistent-file-that-does-not-exist.xyz 2>/dev/null; then
    echo "FAIL: git add non-existent file returned 0"; FAIL=$((FAIL+1))
else
    echo "PASS: git add non-existent file returned error"; PASS=$((PASS+1))
fi

section "9. add file outside repo"
mkdir -p /tmp/git-test/outside
echo "outside" > /tmp/git-test/outside/ext.txt
if git -C /tmp/git-test/add-test add /tmp/git-test/outside/ext.txt 2>/dev/null; then
    echo "INFO: git add with absolute path outside repo returned 0"
else
    echo "PASS: git add outside repo returned error"; PASS=$((PASS+1))
fi

echo
echo "RESULT: PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_ADD_ALL_PASSED"
else
    echo "GIT_ADD_HAS_FAILURES"
fi
