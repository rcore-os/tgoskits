#!/bin/sh
# run-monitor.sh -- on-target launcher for the StarryOS `monitor` app carpet.
#
# Sets the musl dynamic-loader search path (so the loader finds the glances CPython .so closure
# injected under /lib + /usr/lib), puts the app's python/ + programs/ on PYTHONPATH, forces a
# deterministic environment, then hands off to python3 run_monitor.py which runs the prometheus +
# glances carpets and emits the PASS/FAIL gate. The `TEST PASSED` / `MONITOR_OK=N/N` anchor lives
# ONLY in run_monitor.py, never in this wrapper, so the success regex cannot self-match the launch
# command echoed on the serial console.
#
# The musl loader reads only /etc/ld-musl-<this-arch>.path; writing all four names is harmless and
# keeps the launcher arch-agnostic.
for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path"
done

export PATH=/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
export GLANCES_BIN=glances
# prometheus stack binaries (injected by prebuild.sh)
export PROMETHEUS_BIN=/usr/local/bin/prometheus
export PROMTOOL_BIN=/usr/local/bin/promtool
export NODE_EXPORTER_BIN=/usr/bin/node_exporter
export PROMETHEUS_CFG=/etc/prometheus.yml
# grafana (injected by prebuild.sh)
export GRAFANA_BIN=/opt/grafana/bin/grafana
export GRAFANA_HOMEPATH=/opt/grafana

cd /root/monitor/python || exit 1
exec python3 run_monitor.py
