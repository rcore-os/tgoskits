#!/bin/sh
set -eu

/bin/busybox rm -f /dev/console /dev/null
/bin/busybox mknod -m 0600 /dev/console c 5 1
/bin/busybox mknod -m 0666 /dev/null c 1 3
exec </dev/console >/dev/console 2>&1

echo TASK2_INIT_START
/bin/busybox ifconfig eth0 10.0.42.15 netmask 255.255.255.0 up
echo TASK2_NET_CONFIGURED interface=eth0 ip=10.0.42.15/24
exec /bin/task2-net
