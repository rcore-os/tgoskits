#!/bin/sh
# StarryOS network benchmark: iperf3 client against host server (via SLIRP).
# Host must be running: iperf3 -s -p 5201 (listening on 0.0.0.0).
# QEMU usermode (SLIRP) exposes the host gateway at 10.0.2.2.

HOST_IP="${HOST_IP:-10.0.2.2}"
export HOST_IP

. /usr/bin/net-bench-common.sh
