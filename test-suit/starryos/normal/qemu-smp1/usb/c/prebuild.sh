#!/bin/sh
set -eu


test -f "$STARRY_STAGING_ROOT/usr/include/libusb-1.0/libusb.h"
test -f "$STARRY_STAGING_ROOT/usr/lib/pkgconfig/libusb-1.0.pc"
