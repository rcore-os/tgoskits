#!/bin/sh
echo "=== install git ==="
apk add git || { echo "GIT_TEST_FAILED"; exit 1; }

echo "=== test git workflow ===" &&
mkdir -p /tmp/test-repo && cd /tmp/test-repo &&
git init &&
git config user.email "test@starry.os" &&
git config user.name "Test" &&
echo "hello" > hello.txt &&
git add hello.txt &&
git commit -m "initial commit" &&
git log --oneline &&
git branch feature &&
git checkout feature &&
echo "feature" > feature.txt &&
git add feature.txt &&
git commit -m "add feature" &&
git checkout master &&
git merge feature &&
echo "modified" >> hello.txt &&
git diff &&
git status &&
echo "GIT_TEST_PASSED" || { echo "GIT_TEST_FAILED"; exit 1; }
