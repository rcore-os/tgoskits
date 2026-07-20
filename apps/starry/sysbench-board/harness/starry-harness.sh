#!/bin/sh
# StarryOS-side controlled harness. Deployed to the board rootfs
# (/usr/local/bin/starry-harness.sh) and run by StarryOS via the uboot-config
# shell_init_cmd. POSIX sh only (no bashisms) — the StarryOS /bin/sh is dash.
# StarryOS has no cpufreq, so no frequency control here; cpuprobe measures the
# actual frequency directly instead.
SB=/usr/bin/sysbench
CP=/usr/local/bin/cpuprobe
MB=/usr/local/bin/membw
PRIME=20000

test -x "$SB" || { echo SYSBENCH_MISSING; exit 1; }
echo HARNESS_STARRY_BEGIN
echo "HS_UNAME $(uname -r 2>/dev/null) $(uname -m 2>/dev/null)"

echo "== per-core cpuprobe (DIRECT: midr=core-type, mhz_pmc=freq, landed=affinity) =="
for c in 0 1 2 3 4 5 6 7; do
  if [ -x "$CP" ]; then
    echo "HS_PC $($CP $c 2>/dev/null | sed 's/^CPUPROBE //')"
  else
    echo "HS_PC req=$c no_cpuprobe"
  fi
done

echo "== per-core pinned sysbench cpu (does taskset reach faster cores?) =="
for c in 0 1 2 3 4 5 6 7; do
  ev=$(taskset -c $c "$SB" cpu --cpu-max-prime=$PRIME --threads=1 --time=3 run 2>/dev/null | awk '/events per second/{print $4}')
  echo "HS_PSB c=$c ev=${ev:-NA}"
done

echo "== per-core membw (isolate the 200x memory gap) =="
for c in 0 4 7; do
  if [ -x "$MB" ]; then
    echo "HS_PM $($MB $c 128 2>/dev/null | sed 's/^MEMBW //')"
  else
    echo "HS_PM core=$c no_membw"
  fi
done

echo "== sysbench matrix =="
for t in 1 2 4; do
  echo "HS_MX cpu t=$t ev=$($SB cpu --cpu-max-prime=$PRIME --threads=$t --time=5 run 2>/dev/null | awk '/events per second/{print $4}')"
done
echo "HS_MX thr t=4 ev=$($SB threads --threads=4 --time=5 run 2>/dev/null | awk '/total number of events/{print $5}')"
echo "HS_MX mutex t=4 $($SB mutex --threads=4 --mutex-num=4096 run 2>/dev/null | awk '/total time/{print $3}')"
echo "HS_MX mem $($SB memory --threads=4 --memory-total-size=1G run 2>/dev/null | awk '/transferred/{print}')"

echo "== memory param sweep (block size x oper, single thread) =="
for bs in 1K 1M; do
  for op in read write; do
    echo "HS_MEMSW bs=$bs op=$op $($SB memory --threads=1 --memory-block-size=$bs --memory-oper=$op --memory-total-size=2G run 2>/dev/null | awk '/transferred/{print}')"
  done
done

sync 2>/dev/null
echo SYSBENCH_BOARD_DONE
