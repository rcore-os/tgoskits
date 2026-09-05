#!/usr/bin/env bash
# Linux-side controlled harness. Run ON the board Linux:
#   ssh orangepi@<ip> 'bash -s' < linux-harness.sh | tee linux-harness.out
# Needs sudo for cpufreq control (board sudo password = orangepi).
#
# Produces the REFERENCE data the decomposition needs:
#   - per-core MIDR map (which logical cpu is A55 vs A76)
#   - ips-vs-frequency and sysbench-vs-frequency curves for A55 and A76
#     (so any StarryOS ips can be mapped back to an exact frequency)
#   - per-core cpuprobe + membw at the default governor
#   - the full sysbench matrix
set -uo pipefail
SB=${SB:-/usr/bin/sysbench}
CP=${CP:-/usr/local/bin/cpuprobe}
MB=${MB:-/usr/local/bin/membw}
PRIME=${PRIME:-20000}
PW=${BOARD_PW:-orangepi}
priv() { echo "$PW" | sudo -S "$@" 2>/dev/null; }

echo "HARNESS_LINUX_BEGIN"
echo "HL_UNAME $(uname -r) $(uname -m)"
for pair in "$SB sysbench" "$CP cpuprobe" "$MB membw"; do
  set -- $pair; [ -x "$1" ] || echo "HL_MISSING $2 ($1)"
done

echo "== core map (0xd05=A55 0xd0b=A76) =="
i=0
for part in $(awk '/CPU part/{print $4}' /proc/cpuinfo); do
  echo "HL_CORE cpu$i part=$part"; i=$((i + 1))
done

echo "== reference curves: cpuprobe.ips + sysbench.ev vs frequency, per cluster =="
for cl in 0 4; do
  d=/sys/devices/system/cpu/cpu$cl/cpufreq
  [ -d "$d" ] || { echo "HL_REF cl=$cl no_cpufreq"; continue; }
  priv sh -c "echo userspace > $d/scaling_governor"
  for f in $(cat "$d/scaling_available_frequencies" 2>/dev/null); do
    priv sh -c "echo $f > $d/scaling_setspeed"; sleep 0.25
    cur=$(cat "$d/scaling_cur_freq")
    ips=$([ -x "$CP" ] && taskset -c $cl "$CP" $cl 2>/dev/null | sed -n 's/.* ips=\([0-9]*\).*/\1/p')
    sb=$(taskset -c $cl "$SB" cpu --cpu-max-prime=$PRIME --threads=1 --time=2 run 2>/dev/null | awk '/events per second/{print $4}')
    echo "HL_REF cl=$cl setkhz=$f curkhz=$cur ips=${ips:-NA} sb=${sb:-NA}"
  done
  priv sh -c "echo ondemand > $d/scaling_governor"
done

echo "== per-core probe (default governor) =="
for c in 0 1 2 3 4 5 6 7; do
  [ -x "$CP" ] && echo "HL_PC $($CP $c 2>/dev/null | sed 's/^CPUPROBE //')"
done
for c in 0 4; do
  [ -x "$MB" ] && echo "HL_PM $($MB $c 256 2>/dev/null | sed 's/^MEMBW //')"
done

echo "== sysbench matrix =="
for t in 1 2 4 8; do
  echo "HL_MX cpu t=$t ev=$($SB cpu --cpu-max-prime=$PRIME --threads=$t --time=5 run 2>/dev/null | awk '/events per second/{print $4}')"
done
for t in 4 8; do
  echo "HL_MX thr t=$t ev=$($SB threads --threads=$t --time=5 run 2>/dev/null | awk '/total number of events/{print $5}')"
done
echo "HL_MX mutex t=8 $($SB mutex --threads=8 --mutex-num=4096 run 2>/dev/null | awk '/total time/{print $3}')"
echo "HL_MX mem $($SB memory --threads=4 --memory-total-size=4G run 2>/dev/null | awk '/transferred/{print}')"
echo "HARNESS_LINUX_END"
