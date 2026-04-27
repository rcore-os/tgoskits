#!/bin/sh
PASS=0; FAIL=0; SKIP=0
# busybox_cpio

_t=$({ timeout 10 sh -c 'busybox echo in | busybox cpio -o -H newc 2>/dev/null | busybox cpio -i -H newc 2>/dev/null && busybox echo cpio_ok'; } 2>&1)
if echo "$_t" | grep -qF "cpio_ok"; then echo "PASS: busybox_cpio"; PASS=$((PASS+1)); else echo "FAIL: busybox_cpio"; FAIL=$((FAIL+1)); fi

# busybox_tar

_t=$({ timeout 10 sh -c 'busybox mkdir -p /tmp/bb_tar_d && busybox echo one > /tmp/bb_tar_d/f && busybox tar -cf /tmp/bb_tar.tar -C /tmp/bb_tar_d . && busybox tar -tf /tmp/bb_tar.tar'; } 2>&1)
if echo "$_t" | grep -qF "./f"; then echo "PASS: busybox_tar"; PASS=$((PASS+1)); else echo "FAIL: busybox_tar"; FAIL=$((FAIL+1)); fi

echo "PASS: $PASS  FAIL: $FAIL  SKIP: $SKIP"