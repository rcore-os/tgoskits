#!/bin/bash
# Build the minimal aarch64 ext4 rootfs (mini.img + mini2.img) for the two guests.
set -e
cd "$(dirname "$0")"
BB=$(pwd)/busybox-aarch64
IVC=$(pwd)/ivcproto/target/aarch64-unknown-linux-musl/release/ivcproto
INIT=$(pwd)/miniinit.sh
LDMUSL=$(pwd)/ld-musl-aarch64.so.1
rm -rf miniroot mini.img mini2.img
fakeroot bash -c "
  set -e
  mkdir -p miniroot/bin miniroot/dev miniroot/proc miniroot/sys miniroot/sbin miniroot/lib
  install -m 755 '$BB' miniroot/bin/busybox
  for ap in sh mount ls cat cut tr ip ifconfig sleep poweroff ln mkdir grep; do ln -s busybox miniroot/bin/\$ap; done
  install -m 755 '$LDMUSL' miniroot/lib/ld-musl-aarch64.so.1
  ln -s ld-musl-aarch64.so.1 miniroot/lib/libc.musl-aarch64.so.1
  install -m 755 '$IVC' miniroot/ivcproto
  install -m 755 '$INIT' miniroot/init
  mknod -m 622 miniroot/dev/console c 5 1
  mknod -m 666 miniroot/dev/null    c 1 3
  mknod -m 666 miniroot/dev/tty     c 5 0
  mknod -m 666 miniroot/dev/ttyAMA0 c 204 64
  mke2fs -q -F -t ext4 -L minirootfs -d miniroot mini.img 256M
"
cp mini.img mini2.img
echo "built mini.img + mini2.img"
