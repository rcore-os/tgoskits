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
      echo "usage: sh scripts/run-offline-monitor.sh [--out-dir DIR] [--interval-sec N] [--duration-sec N]"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
MONITOR_SH=${MONITOR_SH:-"$SCRIPT_DIR/../system-monitor.sh"}
if [ ! -f "$MONITOR_SH" ] && [ -f /usr/bin/system-monitor.sh ]; then
  MONITOR_SH=/usr/bin/system-monitor.sh
fi

if [ ! -f "$MONITOR_SH" ]; then
  echo "offline-monitor: system-monitor.sh not found" >&2
  echo OFFLINE_MONITOR_FAILED
  exit 1
fi

mkdir -p "$OUT_DIR"

if ! sh "$MONITOR_SH" \
  --out-dir "$OUT_DIR" \
  --interval-sec "$INTERVAL_SEC" \
  --duration-sec "$DURATION_SEC"; then
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
