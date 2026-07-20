#!/bin/sh
# Identical sysbench matrix for a Linux-vs-StarryOS comparison on the same board
# and same binary (glibc /usr/bin/sysbench 1.0.20). POSIX sh. Run on StarryOS via
# the serial shell and on board Linux via ssh; the `CMP_*` lines are the data.
SB=/usr/bin/sysbench
P=20000
T=8
test -x "$SB" || { echo SYSBENCH_MISSING; exit 1; }
echo "CMP_BEGIN $(uname -s) $(uname -r) $(uname -m)"

# CPU throughput scaling (the headline: events/sec vs thread count).
for t in 1 2 4 8; do
  ev=$($SB cpu --cpu-max-prime=$P --threads=$t --time=$T run 2>/dev/null | awk '/events per second/{print $4}')
  echo "CMP cpu t=$t ev=${ev:-NA}"
done

# Scheduler-heavy: thread yields, and mutex contention.
ev=$($SB threads --threads=8 --thread-yields=1000 --thread-locks=8 --time=$T run 2>/dev/null | awk '/total number of events/{print $NF}')
echo "CMP threads t=8 events=${ev:-NA}"
tt=$($SB mutex --threads=8 --mutex-num=4096 --mutex-locks=50000 run 2>/dev/null | awk '/total time/{print $NF}')
echo "CMP mutex t=8 total_time=${tt:-NA}"

# Memory bandwidth (1 MiB blocks, write then read).
for op in write read; do
  line=$($SB memory --threads=8 --memory-block-size=1M --memory-oper=$op --memory-total-size=8G run 2>/dev/null | awk '/transferred/{print}')
  echo "CMP mem $op ${line:-NA}"
done

echo CMP_DONE
