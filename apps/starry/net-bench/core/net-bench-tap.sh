#!/bin/sh
# StarryOS network benchmark via TAP: guest client -> host server.
# Host must run: iperf3 -s -p 5201 -B 192.168.100.1
# Static ARP is normally unnecessary; add it only when debugging ARP issues.

HOST_IP="${HOST_IP:-192.168.100.1}"
export HOST_IP

. /usr/bin/net-bench-common.sh
