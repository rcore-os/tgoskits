#!/bin/sh
set -eu

OUT_DIR=/root/monitor
mkdir -p "$OUT_DIR"

sh /usr/bin/system-monitor.sh \
  --out-dir "$OUT_DIR" \
  --interval-sec 1 \
  --duration-sec 60

echo STARRY_SYSTEM_METRICS_BEGIN
cat "$OUT_DIR/system_metrics.csv"
echo STARRY_SYSTEM_METRICS_END

echo STARRY_EVENTS_BEGIN
cat "$OUT_DIR/events.jsonl"
echo STARRY_EVENTS_END
