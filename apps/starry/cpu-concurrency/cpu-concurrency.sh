#!/bin/sh
# On-target runner: execute the cpu-concurrency carpet. The binary prints one
# "PASS <name>" line per assertion and a final CPU_CONCURRENCY_PASSED (rc 0) or
# CPU_CONCURRENCY_FAILED (rc!=0) marker that the qemu gate matches. StarryOS runs a
# single vCPU, so these validate cooperative-concurrency correctness, not throughput.
set -u
exec /usr/bin/cpu-concurrency
