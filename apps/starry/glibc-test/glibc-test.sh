#!/bin/sh
/usr/bin/glibc-test
echo "GLIBC_TEST_DONE RC=$?"
/usr/bin/proc-self-exe-test
echo "PROC_SELF_EXE_TEST_DONE RC=$?"
