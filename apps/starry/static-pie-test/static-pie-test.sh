#!/bin/sh
echo "BEFORE_BINARY"
/usr/bin/static-pie-test
RC=$?
echo "AFTER_BINARY RC=$RC"
if [ $RC -eq 0 ]; then
    echo "STATIC_PIE_TEST_PASSED"
else
    echo "STATIC_PIE_TEST_FAILED: RC=$RC"
fi
