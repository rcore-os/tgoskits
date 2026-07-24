#!/bin/sh
# StarryOS-side controlled harness. Deployed to the board rootfs
# (/usr/local/bin/starry-harness.sh) and run by StarryOS via the uboot-config
# shell_init_cmd. POSIX sh only (no bashisms) — the StarryOS /bin/sh is dash.
# StarryOS has no cpufreq, so no frequency control here; cpuprobe measures the
# actual frequency directly instead.
#
# Failure propagation: every sysbench workload runs through run_sb(), which flags
# a non-zero exit. If ANY workload failed, the harness prints the anchored
# SYSBENCH_BOARD_FAILED marker (a fail_regex in board-orangepi-5-plus.toml) and
# exits non-zero, instead of the SYSBENCH_BOARD_DONE success sentinel — so ostool
# never scores a failed sysbench run as a pass. (Previously each workload's output
# went straight into `$(... | awk)`, discarding the exit code, and the DONE
# sentinel was printed unconditionally.)
SB=/usr/bin/sysbench
CP=/usr/local/bin/cpuprobe
MB=/usr/local/bin/membw
PRIME=20000

test -x "$SB" || { echo SYSBENCH_MISSING; exit 1; }
echo HARNESS_STARRY_BEGIN
echo "HS_UNAME $(uname -r 2>/dev/null) $(uname -m 2>/dev/null)"

fail=0
# run_sb <tag> <cmd...>: run one sysbench workload, capturing its stdout in $OUT
# (no pipe, so $? is the workload's real exit status). On a non-zero exit, emit a
# per-workload diagnostic and raise the fail flag checked before the final
# sentinel. Callers awk the metric out of $OUT afterwards.
run_sb() {
  _tag=$1
  shift
  OUT=$("$@" 2>/dev/null)
  _rc=$?
  if [ "$_rc" -ne 0 ]; then
    echo "HS_WORKLOAD_FAIL $_tag rc=$_rc"
    fail=1
  fi
}

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
  run_sb "psb c=$c" taskset -c $c "$SB" cpu --cpu-max-prime=$PRIME --threads=1 --time=3 run
  ev=$(printf '%s\n' "$OUT" | awk '/events per second/{print $4}')
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
  run_sb "cpu t=$t" "$SB" cpu --cpu-max-prime=$PRIME --threads=$t --time=5 run
  echo "HS_MX cpu t=$t ev=$(printf '%s\n' "$OUT" | awk '/events per second/{print $4}')"
done
run_sb "thr t=4" "$SB" threads --threads=4 --time=5 run
echo "HS_MX thr t=4 ev=$(printf '%s\n' "$OUT" | awk '/total number of events/{print $5}')"
run_sb "mutex t=4" "$SB" mutex --threads=4 --mutex-num=4096 run
echo "HS_MX mutex t=4 $(printf '%s\n' "$OUT" | awk '/total time/{print $3}')"
run_sb "mem t=4" "$SB" memory --threads=4 --memory-total-size=1G run
echo "HS_MX mem $(printf '%s\n' "$OUT" | awk '/transferred/{print}')"

echo "== memory param sweep (block size x oper, single thread) =="
for bs in 1K 1M; do
  for op in read write; do
    run_sb "mem bs=$bs op=$op" "$SB" memory --threads=1 --memory-block-size=$bs --memory-oper=$op --memory-total-size=2G run
    echo "HS_MEMSW bs=$bs op=$op $(printf '%s\n' "$OUT" | awk '/transferred/{print}')"
  done
done

sync 2>/dev/null
if [ "$fail" -ne 0 ]; then
  echo SYSBENCH_BOARD_FAILED
  exit 1
fi
echo SYSBENCH_BOARD_DONE
