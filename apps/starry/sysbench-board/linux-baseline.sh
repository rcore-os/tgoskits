#!/usr/bin/env bash
# Native-Linux baseline — run this ON THE BOARD LINUX to capture the same
# subtests StarryOS runs in init.sh, using the SAME sysbench binary on the SAME
# board. That is the apples-to-apples half of the comparison.
#
# Linux boots all 8 cores, so this sweeps threads up to 8; StarryOS is capped by
# the kernel build's max_cpu_num (currently 4). Compare the overlapping points
# (1/2/4) plus StarryOS-4 vs Linux-4/8.
set -uo pipefail

echo SYSBENCH_LINUX_BEGIN
for t in 1 2 4 8; do
  echo "CPU_THREADS=$t"
  sysbench cpu --cpu-max-prime=20000 --threads="$t" --time=5 run 2>&1 \
    | grep -iE 'events per second'
done
echo THREADS_T4
sysbench threads --threads=4 --time=5 run 2>&1 | grep -iE 'events per second'
echo MUTEX_T4
sysbench mutex --threads=4 run 2>&1 | grep -iE 'total time'
echo MEMORY_T4
sysbench memory --threads=4 --memory-total-size=1G run 2>&1 | grep -iE 'per second'
echo SYSBENCH_LINUX_DONE
