#!/usr/bin/env bash
set -euo pipefail

case_name=${1:-boot}

case "$case_name" in
  boot)
    cargo starry perf --case boot --timeout 20
    ;;
  blk-read)
    cargo starry perf \
      --case blk-read \
      --qperf-metrics \
      --start-marker QPERF_BEGIN \
      --stop-marker QPERF_END \
      --workload-timeout 45 \
      --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
    ;;
  compare-self)
    python3 tools/starry-syscall-harness/harness.py perf-compare \
      --baseline target/qperf/blk-read/perf/riscv64/latest/report.json \
      --candidate target/qperf/blk-read/perf/riscv64/latest/report.json \
      --name blk-self-smoke \
      --output-dir target/qperf/blk-read/compare-self
    ;;
  *)
    cat >&2 <<'EOF'
usage: tools/starry-syscall-harness/scripts/qperf-smoke.sh [boot|blk-read|compare-self]
EOF
    exit 2
    ;;
esac
