#!/bin/sh
echo "=== install git ==="
apk add git || { echo "GIT_TEST_FAILED"; exit 1; }

echo "=== test git init ==="
mkdir -p /tmp/test-repo && cd /tmp/test-repo
git init
git config user.email "test@starry.os"
git config user.name "Test"

echo "=== test git add/commit ==="
echo "hello" > hello.txt
git add hello.txt
git commit -m "initial commit"

echo "=== test git log ==="
git log --oneline

echo "=== test git branch ==="
git branch feature
git checkout feature
echo "feature" > feature.txt
git add feature.txt
git commit -m "add feature"
git checkout master

echo "=== test git merge ==="
git merge feature

echo "=== test git diff ==="
echo "modified" >> hello.txt
git diff

echo "=== test git status ==="
git status

echo "GIT_TEST_PASSED"
