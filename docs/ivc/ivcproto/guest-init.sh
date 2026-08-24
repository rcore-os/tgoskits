#!/bin/sh
/bin/busybox --install -s /bin 2>/dev/null
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
exec >/dev/console 2>&1
echo "MINIINIT up"
# wait for the virtio-net interface to be probed
MAC=""
i=0
while [ $i -lt 20 ]; do
  if [ -e /sys/class/net/eth0/address ]; then
    MAC=$(cat /sys/class/net/eth0/address)
    [ -n "$MAC" ] && break
  fi
  i=$((i+1))
  /bin/busybox sleep 1
done
echo "MINIINIT ifaces=$(ls /sys/class/net 2>/dev/null | tr '\n' ' ') eth0_mac=$MAC"
LAST=$(echo "$MAC" | cut -d: -f6)
case "$LAST" in 01) MYIP=1;; 02) MYIP=2;; *) MYIP=9;; esac
ip addr add 10.0.0.$MYIP/24 dev eth0 2>/dev/null
ip link set eth0 up 2>/dev/null
echo "IVCINIT self=10.0.0.$MYIP MAC=$MAC"
if [ "$MYIP" = "1" ]; then
  /ivcproto server 0.0.0.0:5500 lossy=5
elif [ "$MYIP" = "2" ]; then
  /bin/busybox sleep 5
  /ivcproto client 10.0.0.1:5500 40
fi
echo "IVCINIT done self=10.0.0.$MYIP"
/bin/busybox sleep 2
/bin/busybox poweroff -f 2>/dev/null
while true; do /bin/busybox sleep 5; done
