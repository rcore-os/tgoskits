#!/bin/sh
echo "=== GLIBC BASIC TEST ==="
/usr/bin/glibc-dynamic-smoke
echo "GLIBC_TEST_DONE RC=$?"

echo "=== PROC_SELF_EXE TEST ==="
/usr/bin/proc-self-exe-test
echo "PROC_SELF_EXE_TEST_DONE RC=$?"

echo "=== PTHREAD TEST ==="
/usr/bin/pthread-test
echo "PTHREAD_TEST_DONE RC=$?"

echo "=== REGEX TEST ==="
/usr/bin/regex-test
echo "REGEX_TEST_DONE RC=$?"

echo "=== ALL TESTS COMPLETED ==="
