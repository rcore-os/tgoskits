#!/bin/sh
set -u
suite=STARRY_NATIVE_NETWORK
rtnl_marker=STARRY_RTNL
ok=1
iperf-smoke.sh "$1" || ok=0
printf '\n%s_BEGIN\n' "$rtnl_marker"
rtnl_ok=1
ip addr del 192.168.10.2/24 dev eth1 >/tmp/starry-rtnl-cleanup 2>&1 || true
ip link show dev eth1 || rtnl_ok=0
if ip link show dev starry_missing0 >/tmp/starry-rtnl-missing 2>&1; then
  echo "${rtnl_marker}_BAD_MISSING_LINK"
  rtnl_ok=0
else
  cat /tmp/starry-rtnl-missing
fi

ip addr add 192.168.10.2/24 dev eth1 || rtnl_ok=0
ip -o addr show dev eth1 || rtnl_ok=0
ip addr del 192.168.10.2/24 dev eth1 || rtnl_ok=0
if ip addr del 192.168.10.2/24 dev eth1 >/tmp/starry-rtnl-del-missing 2>&1; then
  echo "${rtnl_marker}_BAD_DELADDR_MISSING"
  rtnl_ok=0
else
  cat /tmp/starry-rtnl-del-missing
fi

if ip -o addr show dev eth1 | grep -q '192\.168\.10\.2/24'; then
  echo "${rtnl_marker}_BAD_DELADDR"
  rtnl_ok=0
fi

ip addr add 192.168.10.2/24 dev eth1 || rtnl_ok=0
ip -o addr show dev eth1 || rtnl_ok=0

if [ "$rtnl_ok" = "1" ]; then
  echo "${rtnl_marker}_OK"
else
  echo "${rtnl_marker}_FAILED"
  ok=0
fi
printf '\n%s_DONE\n' "$rtnl_marker"

if [ "$ok" = "1" ]; then
  echo ${suite}_OK
else
  echo ${suite}_FAILED
fi

[ "$ok" = 1 ]
