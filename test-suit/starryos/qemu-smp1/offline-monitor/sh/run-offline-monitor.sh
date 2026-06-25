#!/bin/sh
set -eu

OUT_DIR=/root/monitor
mkdir -p "$OUT_DIR"

if [ ! -f /usr/bin/system-monitor.sh ]; then
  echo "offline-monitor: missing /usr/bin/system-monitor.sh" >&2
  echo OFFLINE_MONITOR_FAILED
  exit 1
fi

if ! sh /usr/bin/system-monitor.sh \
  --out-dir "$OUT_DIR" \
  --interval-sec 1 \
  --duration-sec 5; then
  echo "offline-monitor: system monitor failed" >&2
  echo OFFLINE_MONITOR_FAILED
  exit 1
fi

if [ ! -f "$OUT_DIR/system_metrics.csv" ] || [ ! -f "$OUT_DIR/events.jsonl" ]; then
  echo "offline-monitor: expected log files were not generated" >&2
  echo OFFLINE_MONITOR_FAILED
  exit 1
fi

echo STARRY_SYSTEM_METRICS_BEGIN
cat "$OUT_DIR/system_metrics.csv"
echo STARRY_SYSTEM_METRICS_END

echo STARRY_EVENTS_BEGIN
cat "$OUT_DIR/events.jsonl"
echo STARRY_EVENTS_END
