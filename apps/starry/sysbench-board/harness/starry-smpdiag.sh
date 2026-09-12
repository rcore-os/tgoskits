#!/bin/sh
# SMP-scaling diagnostic (StarryOS side). Answers: do unpinned sysbench threads
# spread across cores, and can the cores run the workload concurrently at all?
# POSIX sh (dash). Deployed to /usr/local/bin, run via the shell_init_cmd.
SB=/usr/bin/sysbench
P=20000
T=5
test -x "$SB" || { echo SYSBENCH_MISSING; exit 1; }
echo SMPDIAG_BEGIN
echo "SD_UNAME $(uname -r) $(uname -m)"

# 1) Unpinned scaling curve: if flat, threads are not spreading.
for t in 1 2 4 8; do
  ev=$($SB cpu --cpu-max-prime=$P --threads=$t --time=$T run 2>/dev/null | awk '/events per second/{print $4}')
  echo "SD_UNPIN t=$t ev=${ev:-NA}"
done

# 2) Explicit multi-core affinity masks: does a mask make them spread?
ev=$(taskset -c 0-7 $SB cpu --cpu-max-prime=$P --threads=8 --time=$T run 2>/dev/null | awk '/events per second/{print $4}')
echo "SD_MASK all8 t=8 ev=${ev:-NA}"
ev=$(taskset -c 4-7 $SB cpu --cpu-max-prime=$P --threads=4 --time=$T run 2>/dev/null | awk '/events per second/{print $4}')
echo "SD_MASK a76x4 t=4 ev=${ev:-NA}"

# 3) Forced distribution: 4 concurrent single-thread jobs, each pinned to a
#    distinct A76 core. If the cores CAN run concurrently, the four SD_CONC
#    values each read ~full per-core throughput (sum ~= 4x a single core), which
#    is the ceiling a load balancer would unlock. If they instead each read ~1/4,
#    the cores cannot run concurrently (a deeper serialization, not placement).
for c in 4 5 6 7; do
  taskset -c $c $SB cpu --cpu-max-prime=$P --threads=1 --time=$T run 2>/dev/null \
    | awk -v c=$c '/events per second/{print "SD_CONC c="c" ev="$4}' &
done
wait

echo SMPDIAG_DONE
