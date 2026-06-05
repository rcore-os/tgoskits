#!/bin/sh
# git config probe: normal + edge + error cases
PASS=0; FAIL=0
section() { echo; echo "=== $1 ==="; }

# --- NORMAL ---
section "1. set user.name"
git config --global user.name "Test User"
if git config user.name | grep -q "Test User"; then
    echo "PASS: git config set user.name"; PASS=$((PASS+1))
else
    echo "FAIL: git config set user.name"; FAIL=$((FAIL+1))
fi

section "2. set user.email"
git config --global user.email "test@example.com"
if git config user.email | grep -q "test@example.com"; then
    echo "PASS: git config set user.email"; PASS=$((PASS+1))
else
    echo "FAIL: git config set user.email"; FAIL=$((FAIL+1))
fi

# --- EDGE ---
section "3. --list shows configured values"
if git config --list | grep -q "user.name=Test User" && git config --list | grep -q "user.email=test@example.com"; then
    echo "PASS: git config --list shows values"; PASS=$((PASS+1))
else
    echo "FAIL: git config --list"; FAIL=$((FAIL+1))
fi

section "4. per-repo local config override"
rm -rf /tmp/git-test/cfg-local
mkdir -p /tmp/git-test/cfg-local
git init /tmp/git-test/cfg-local
git -C /tmp/git-test/cfg-local config user.name "Local User"
val=$(git -C /tmp/git-test/cfg-local config user.name)
if [ "$val" = "Local User" ]; then
    echo "PASS: git config local override"; PASS=$((PASS+1))
else
    echo "FAIL: git config local override (got: $val)"; FAIL=$((FAIL+1))
fi

section "5. --get-regexp"
if git config --get-regexp "^user\." | grep -q "user.name Test User"; then
    echo "PASS: git config --get-regexp"; PASS=$((PASS+1))
else
    echo "FAIL: git config --get-regexp"; FAIL=$((FAIL+1))
fi

# --- ERROR ---
section "6. read non-existent key"
if git config nonexistent.key 2>/dev/null; then
    echo "FAIL: git config on missing key returned 0"; FAIL=$((FAIL+1))
else
    echo "PASS: git config on missing key returned error"; PASS=$((PASS+1))
fi

section "7. invalid section name"
if git config "invalid section.key" "value" 2>/dev/null; then
    echo "FAIL: git config invalid section name returned 0"; FAIL=$((FAIL+1))
else
    echo "PASS: git config invalid section name returned error"; PASS=$((PASS+1))
fi

echo
echo "RESULT: PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "GIT_CONFIG_ALL_PASSED"
else
    echo "GIT_CONFIG_HAS_FAILURES"
fi
exit $((FAIL > 0 ? 1 : 0))
