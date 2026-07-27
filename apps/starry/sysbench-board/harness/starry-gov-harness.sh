#!/bin/sh
# Ondemand-governor validation harness (StarryOS side). Deployed to the board
# rootfs and run via the uboot-config shell_init_cmd. POSIX sh (dash) only.
#
# What it proves, using two independent signals:
#   1. The kernel's own `gov: <cluster> busy=N% opp A->B = M MHz` lines on the
#      serial console (printed by governor_poll on every OPP change). These are
#      the direct record of the governor tracking load — watch them per phase.
#   2. cpuprobe ips. cpuprobe self-loads its core for ~1 s before its measured
#      window, so the 100 ms governor has already scaled that cluster UP by the
#      time it measures: ips reflects the governor-chosen freq, not idle. Compare
#      to the fixed-OPP baseline (A76 1200 ~= 113M ips; A55 1008 baseline) — a
#      clear jump means the governor raised the cluster past its boot OPP.
#
# Arg $1 = max sysbench threads for the PSU-stress phase (2 = ramped/light,
# 8 = all-core). Ramped runs do 2 first, inspect, then 8.
SB=/usr/bin/sysbench
CP=/usr/local/bin/cpuprobe
NT="${1:-2}"
PRIME=20000
# Online CPU count, for the per-core probe (the shipped kernel pins max_cpu_num=4).
NCPU=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
test -x "$SB" || { echo SYSBENCH_MISSING; exit 1; }

# Raised if any sysbench workload exits non-zero. Gates GOV_HARNESS_DONE at the
# end so a crashed/failed sysbench run is scored as a failure, not a pass — the
# governor-validation run must never emit its success sentinel after a failure.
fail=0

probe_all() {
  # cpuprobe every online core; the busy cluster reads its governor-scaled freq.
  c=0
  while [ "$c" -lt "$NCPU" ]; do
    echo "GH_PC $($CP $c 2>/dev/null | sed 's/^CPUPROBE //')"
    c=$((c + 1))
  done
}

echo GOV_HARNESS_BEGIN
echo "GH_UNAME $(uname -r 2>/dev/null) $(uname -m 2>/dev/null) threads=$NT"

# Phase 1: sit idle so the governor steps every cluster DOWN toward its floor.
# Expect `gov: ... opp 2->1`, then `1->0` lines above this marker.
echo "GH_PHASE 1 idle_settle 4s (expect gov step-DOWN)"
sleep 4

# Phase 2: per-core cpuprobe. Each run loads its cluster; the governor scales it
# UP mid-run, so ips reads the raised freq. Expect `gov: ... opp ->top` lines.
echo "GH_PHASE 2 cpuprobe_sweep (expect gov step-UP; ips = scaled freq)"
probe_all

# Phase 3: brief idle again (governor steps back down between the sweep and load).
echo "GH_PHASE 3 idle_gap 3s (expect gov step-DOWN)"
sleep 3

# Phase 4: sustained multicore load. N=2 lights ~one cluster; N=8 lights all —
# the PSU-stress step. Survival to the sentinel + no board reset == caps held.
echo "GH_PHASE 4 sysbench cpu threads=$NT time=12 (expect gov step-UP; PSU stress)"
# Run without a pipe so $? is sysbench's real exit status; awk the metric after.
sb_out=$($SB cpu --cpu-max-prime=$PRIME --threads=$NT --time=12 run 2>/dev/null)
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "GH_SB_FAIL rc=$rc"
  fail=1
fi
printf '%s\n' "$sb_out" | awk '/events per second/{print "GH_SB eps="$4}'

# Phase 5: settle back to idle; governor should shed frequency again.
echo "GH_PHASE 5 idle_return 4s (expect gov step-DOWN)"
sleep 4
echo "GH_FINAL_PROBE"
probe_all

sync 2>/dev/null
if [ "$fail" -ne 0 ]; then
  echo GOV_HARNESS_FAILED
  exit 1
fi
echo GOV_HARNESS_DONE
