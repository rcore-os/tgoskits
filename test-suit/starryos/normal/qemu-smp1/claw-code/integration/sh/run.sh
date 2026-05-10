#!/bin/sh
echo "=== Smoke: claw --help ==="
/usr/bin/claw --help
echo "EXIT:$?"
echo "=== Diagnostic: claw version ==="
/usr/bin/claw version
echo "EXIT:$?"
echo "=== Diagnostic: claw doctor ==="
/usr/bin/claw doctor 2>&1
echo "EXIT:$?"
