#!/bin/sh
# git commit probe: normal + edge + error cases
PASS=0; FAIL=0
section() { echo; echo "=== $1 ==="; }

rm -rf /tmp/git-test/commit-test
mkdir -p /tmp/git-test/commit-test
git init /tmp/git-test/commit-test
cd /tmp/git-test/commit-test
git config user.name "Test User"
git config user.email "test@example.com"

# --- NORMAL ---
section "1. basic commit"
echo "content" > file.txt
git add file.txt
if git commit -m "initial commit" 2>&1 | grep -qE '\[master \(root-commit\)|\[main \(root-commit\)'; then
    echo "PASS: git commit basic"; PASS=$((PASS+1))
else
    echo "FAIL: git commit basic"; FAIL=$((FAIL+1))
fi

section "2. second commit"
echo "more" >> file.txt
git add file.txt
if git commit -m "second commit" 2>&1 | grep -q '1 file changed'; then
    echo "PASS: git commit second"; PASS=$((PASS+1))
else
    echo "FAIL: git commit second"; FAIL=$((FAIL+1))
fi

section "3. commit with author override"
echo "author" > author.txt
git add author.txt
if git commit --author="Other <other@test.com>" -m "author override" 2>&1; then
    if git log -1 --format="%an <%ae>" | grep -q "Other <other@test.com>"; then
        echo "PASS: git commit --author"; PASS=$((PASS+1))
    else
        echo "FAIL: git commit --author (author mismatch)"; FAIL=$((FAIL+1))
    fi
else
    echo "FAIL: git commit --author (commit failed)"; FAIL=$((FAIL+1))
fi

section "4. multi-line commit message"
echo "multi" > multi.txt
git add multi.txt
if git commit -m "line one" -m "line two" 2>&1; then
    echo "PASS: git commit multi-line message"; PASS=$((PASS+1))
else
    echo "FAIL: git commit multi-line message"; FAIL=$((FAIL+1))
fi

# --- EDGE ---
section "5. commit empty file change"
touch empty-file.txt
git add empty-file.txt
if git commit -m "add empty file" 2>&1; then
    echo "PASS: git commit empty file"; PASS=$((PASS+1))
else
    echo "FAIL: git commit empty file"; FAIL=$((FAIL+1))
fi

section "6. commit deletion"
rm file.txt
git add file.txt
if git commit -m "delete file" 2>&1 | grep -q 'file changed'; then
    echo "PASS: git commit deletion"; PASS=$((PASS+1))
else
    echo "FAIL: git commit deletion"; FAIL=$((FAIL+1))
fi

section "7. commit with no changes"
if git commit --allow-empty -m "empty commit" 2>&1; then
    echo "PASS: git commit --allow-empty"; PASS=$((PASS+1))
else
    echo "FAIL: git commit --allow-empty"; FAIL=$((FAIL+1))
fi

# --- ERROR ---
section "8. commit without staging (when no changes)"
if git commit -m "nothing" 2>/dev/null; then
    echo "INFO: git commit with no staged changes returned 0"
else
    echo "PASS: git commit with no staged changes returned error"; PASS=$((PASS+1))
fi

echo
echo "RESULT: PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_COMMIT_ALL_PASSED"
else
    echo "GIT_COMMIT_HAS_FAILURES"
fi
