#!/bin/sh
set -eu

apk add binutils gcc musl-dev libusb-dev

test -f "$STARRY_STAGING_ROOT/usr/include/libusb-1.0/libusb.h"
test -f "$STARRY_STAGING_ROOT/usr/lib/pkgconfig/libusb-1.0.pc"
