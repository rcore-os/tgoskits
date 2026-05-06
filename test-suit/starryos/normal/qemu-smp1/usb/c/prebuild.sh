#!/bin/sh
set -eu

apk add binutils gcc musl-dev libusb-dev usbutils eudev hwdata

test -f "$STARRY_STAGING_ROOT/usr/include/libusb-1.0/libusb.h"
test -f "$STARRY_STAGING_ROOT/usr/lib/pkgconfig/libusb-1.0.pc"
test -x "$STARRY_STAGING_ROOT/usr/bin/lsusb"
test -x "$STARRY_STAGING_ROOT/bin/udevadm"
"$STARRY_STAGING_ROOT/bin/udevadm" hwdb --update --usr --root="$STARRY_STAGING_ROOT"
test -f "$STARRY_STAGING_ROOT/usr/lib/udev/hwdb.bin"
