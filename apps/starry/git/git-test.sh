#!/bin/sh
export GIT_PAGER=cat
export PAGER=cat
export TERM=dumb

command -v git >/dev/null 2>&1 || { echo "GIT_TEST_FAILED"; exit 1; }

echo "=== test git workflow ===" &&
mkdir -p /tmp/test-repo && cd /tmp/test-repo &&
git init -b main &&
git config user.email "test@starry.os" &&
git config user.name "Test" &&
git config color.ui false &&
echo "hello" > hello.txt &&
git add hello.txt &&
git commit -m "initial commit" &&
git --no-pager log --oneline &&
git branch feature &&
git checkout feature &&
echo "feature" > feature.txt &&
git add feature.txt &&
git commit -m "add feature" &&
git checkout main &&
git merge feature &&
echo "modified" >> hello.txt &&
git --no-pager diff &&
git status --short &&
echo "GIT_TEST_PASSED" || { echo "GIT_TEST_FAILED"; exit 1; }
