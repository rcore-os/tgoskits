#!/bin/sh
set -eu

OUT_DIR=/root/monitor
INTERVAL_SEC=1
DURATION_SEC=60

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out-dir)
      OUT_DIR=$2
      shift 2
      ;;
    --interval-sec)
      INTERVAL_SEC=$2
      shift 2
      ;;
    --duration-sec)
      DURATION_SEC=$2
      shift 2
      ;;
    -h|--help)
      echo "usage: sh system-monitor.sh [--out-dir DIR] [--interval-sec N] [--duration-sec N]"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

mkdir -p "$OUT_DIR"
SYSTEM_CSV="$OUT_DIR/system_metrics.csv"
EVENTS_JSONL="$OUT_DIR/events.jsonl"

HEADER="timestamp_ms,uptime_ms,cpu_total_pct,cpu0_pct,cpu1_pct,cpu2_pct,cpu3_pct,cpu4_pct,cpu5_pct,cpu6_pct,cpu7_pct,run_queue_len,ctxt_delta,irq_delta,mem_total_kib,mem_used_kib,mem_free_kib,mem_peak_kib,page_alloc_delta,page_free_delta,fs_read_delta,fs_write_delta"
printf '%s\n' "$HEADER" > "$SYSTEM_CSV"
printf '{"timestamp_ms":0,"level":"info","source":"monitor","event":"start","message":"system monitor started"}\n' > "$EVENTS_JSONL"

read_cpu_line() {
  key=$1
  awk -v key="$key" '$1 == key {print $2, $3, $4, $5, $6, $7, $8, $9, $10, $11}' /proc/stat 2>/dev/null || true
}

sum_cpu() {
  echo "$1" | awk '{s=0; for (i=1; i<=NF; i++) s += $i; print s}'
}

idle_cpu() {
  echo "$1" | awk '{print $4 + $5}'
}

pct_cpu() {
  prev=$1
  curr=$2
  if [ -z "$prev" ] || [ -z "$curr" ]; then
    printf 'NA'
    return
  fi
  prev_total=$(sum_cpu "$prev")
  curr_total=$(sum_cpu "$curr")
  prev_idle=$(idle_cpu "$prev")
  curr_idle=$(idle_cpu "$curr")
  awk -v pt="$prev_total" -v ct="$curr_total" -v pi="$prev_idle" -v ci="$curr_idle" '
    BEGIN {
      total = ct - pt;
      idle = ci - pi;
      if (total <= 0) print "NA";
      else printf "%.2f", (total - idle) * 100.0 / total;
    }'
}

stat_value() {
  key=$1
  awk -v key="$key" '$1 == key {print $2; exit}' /proc/stat 2>/dev/null || true
}

mem_value() {
  key=$1
  awk -v key="$key" '$1 == key ":" {print $2; exit}' /proc/meminfo 2>/dev/null || true
}

uptime_ms() {
  awk '{printf "%d", $1 * 1000}' /proc/uptime 2>/dev/null || printf 'NA'
}

prev_cpu=$(read_cpu_line cpu)
prev_cpu0=$(read_cpu_line cpu0)
prev_cpu1=$(read_cpu_line cpu1)
prev_cpu2=$(read_cpu_line cpu2)
prev_cpu3=$(read_cpu_line cpu3)
prev_cpu4=$(read_cpu_line cpu4)
prev_cpu5=$(read_cpu_line cpu5)
prev_cpu6=$(read_cpu_line cpu6)
prev_cpu7=$(read_cpu_line cpu7)
prev_ctxt=$(stat_value ctxt)
prev_intr=$(stat_value intr)
mem_peak=0

samples=$((DURATION_SEC / INTERVAL_SEC))
if [ "$samples" -lt 1 ]; then
  samples=1
fi

i=0
while [ "$i" -le "$samples" ]; do
  if [ "$i" -gt 0 ]; then
    sleep "$INTERVAL_SEC"
  fi

  curr_cpu=$(read_cpu_line cpu)
  curr_cpu0=$(read_cpu_line cpu0)
  curr_cpu1=$(read_cpu_line cpu1)
  curr_cpu2=$(read_cpu_line cpu2)
  curr_cpu3=$(read_cpu_line cpu3)
  curr_cpu4=$(read_cpu_line cpu4)
  curr_cpu5=$(read_cpu_line cpu5)
  curr_cpu6=$(read_cpu_line cpu6)
  curr_cpu7=$(read_cpu_line cpu7)

  timestamp_ms=$((i * INTERVAL_SEC * 1000))
  uptime=$(uptime_ms)
  cpu_total=$(pct_cpu "$prev_cpu" "$curr_cpu")
  cpu0=$(pct_cpu "$prev_cpu0" "$curr_cpu0")
  cpu1=$(pct_cpu "$prev_cpu1" "$curr_cpu1")
  cpu2=$(pct_cpu "$prev_cpu2" "$curr_cpu2")
  cpu3=$(pct_cpu "$prev_cpu3" "$curr_cpu3")
  cpu4=$(pct_cpu "$prev_cpu4" "$curr_cpu4")
  cpu5=$(pct_cpu "$prev_cpu5" "$curr_cpu5")
  cpu6=$(pct_cpu "$prev_cpu6" "$curr_cpu6")
  cpu7=$(pct_cpu "$prev_cpu7" "$curr_cpu7")

  run_queue=$(stat_value procs_running)
  [ -n "$run_queue" ] || run_queue=NA

  curr_ctxt=$(stat_value ctxt)
  curr_intr=$(stat_value intr)
  if [ -n "$prev_ctxt" ] && [ -n "$curr_ctxt" ]; then
    ctxt_delta=$((curr_ctxt - prev_ctxt))
  else
    ctxt_delta=NA
  fi
  if [ -n "$prev_intr" ] && [ -n "$curr_intr" ]; then
    irq_delta=$((curr_intr - prev_intr))
  else
    irq_delta=NA
  fi

  mem_total=$(mem_value MemTotal)
  mem_free=$(mem_value MemFree)
  if [ -n "$mem_total" ] && [ -n "$mem_free" ]; then
    mem_used=$((mem_total - mem_free))
    if [ "$mem_used" -gt "$mem_peak" ]; then
      mem_peak=$mem_used
    fi
  else
    mem_total=NA
    mem_used=NA
    mem_free=NA
    mem_peak=NA
  fi

  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,NA,NA,NA,NA\n' \
    "$timestamp_ms" "$uptime" "$cpu_total" "$cpu0" "$cpu1" "$cpu2" "$cpu3" "$cpu4" "$cpu5" "$cpu6" "$cpu7" \
    "$run_queue" "$ctxt_delta" "$irq_delta" "$mem_total" "$mem_used" "$mem_free" "$mem_peak" >> "$SYSTEM_CSV"

  prev_cpu=$curr_cpu
  prev_cpu0=$curr_cpu0
  prev_cpu1=$curr_cpu1
  prev_cpu2=$curr_cpu2
  prev_cpu3=$curr_cpu3
  prev_cpu4=$curr_cpu4
  prev_cpu5=$curr_cpu5
  prev_cpu6=$curr_cpu6
  prev_cpu7=$curr_cpu7
  prev_ctxt=$curr_ctxt
  prev_intr=$curr_intr
  i=$((i + 1))
done

printf '{"timestamp_ms":%s,"level":"info","source":"monitor","event":"stop","message":"system monitor stopped"}\n' "$((samples * INTERVAL_SEC * 1000))" >> "$EVENTS_JSONL"
