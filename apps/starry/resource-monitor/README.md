# StarryOS Resource Monitor Viewer

This directory contains a small user-space helper for collecting and replaying
resource-monitoring data for StarryOS application experiments. It is intended
for offline board/QEMU debugging and demo evidence, not for cloud monitoring.

The feature has three parts:

- `system-monitor.sh`: a POSIX shell collector that samples existing `/proc`
  files and writes local CSV/JSONL logs.
- `scripts/run-offline-monitor.sh`: an optional demo wrapper that runs the
  collector and prints the generated logs between serial-friendly markers.
- `offline-viewer/index.html`: a self-contained static HTML page that imports the
  logs with the browser File API and renders charts/tables without network
  access.

## Scope

This is an application-layer tool for StarryOS user-space experiments. It does
not add kernel counters, drivers, NPU tracing, USB tracing, robot control logic,
online telemetry services, or test-suite regression coverage. When a metric is
not exposed by the current OS interface, the collector writes `NA` instead of
inventing a value.

The viewer can display optional `robot_trace.csv` files when they are provided,
but this change does not include a robot test script. Precise robot/AI stage
tracing requires the application workload to emit those timing fields; this
helper only defines and visualizes the log format.

## Log Files

The viewer accepts the following files for both StarryOS and Linux baseline
runs:

- `system_metrics.csv`
- `robot_trace.csv` (optional; frame-level robot/AI trace)
- `events.jsonl`

`system_metrics.csv` includes fields such as:

```text
timestamp_ms,uptime_ms,cpu_total_pct,cpu0_pct,...,run_queue_len,ctxt_delta,irq_delta,mem_total_kib,mem_used_kib,mem_free_kib,mem_peak_kib,page_alloc_delta,page_free_delta,fs_read_delta,fs_write_delta
```

`events.jsonl` records one JSON object per line, for example monitor start/stop
or device/runtime events.

## Collect Logs

Run the collector inside the target system. If it runs on host Linux, it collects
host Linux data; if it runs inside StarryOS QEMU or a StarryOS board shell, it
collects StarryOS data.

```sh
cd apps/starry/resource-monitor
sh system-monitor.sh \
  --out-dir /root/monitor \
  --interval-sec 1 \
  --duration-sec 60
```

Expected outputs:

```text
/root/monitor/system_metrics.csv
/root/monitor/events.jsonl
```

`--interval-sec` controls the sampling period in seconds. `--duration-sec`
controls the total collection time. `--out-dir` selects where the CSV/JSONL logs
are written.

For a short QEMU or board demo, run the wrapper instead. It prints markers that
make serial-log extraction easier:

```sh
cd apps/starry/resource-monitor
sh scripts/run-offline-monitor.sh \
  --out-dir /root/monitor \
  --interval-sec 1 \
  --duration-sec 5
```

The wrapper prints:

```text
STARRY_SYSTEM_METRICS_BEGIN
...
STARRY_SYSTEM_METRICS_END
STARRY_EVENTS_BEGIN
...
STARRY_EVENTS_END
```

If the collector is missing, exits with an error, or does not generate both log
files, the wrapper prints `OFFLINE_MONITOR_FAILED` and exits non-zero.

## StarryOS QEMU Or Board Usage

This app is not wired into `test-suit/starryos`. To use it in StarryOS QEMU or on
a board, make sure `system-monitor.sh` and, optionally,
`scripts/run-offline-monitor.sh` are present in the guest rootfs or copied into
the running system. Then run one of the commands above from the StarryOS shell.

Export the generated files to the host through any local/offline path available
for the environment:

- serial output copied between the marker lines printed by the wrapper;
- SD card or USB storage;
- TFTP or direct Ethernet copy when local networking is available;
- rootfs file copy after QEMU or board shutdown;
- a QEMU shared directory when the local setup provides one.

## Linux Baseline

Collect Linux baseline logs with the same script and the same sampling settings.
For fair comparisons, use the same board or the same QEMU resource configuration,
the same workload, the same `--interval-sec`, and the same `--duration-sec`.

```sh
cd apps/starry/resource-monitor
sh system-monitor.sh \
  --out-dir logs/offline-monitor/linux \
  --interval-sec 1 \
  --duration-sec 60
```

Host Linux data is useful for viewer smoke testing, but it should not be treated
as a strict performance baseline against StarryOS QEMU. A meaningful comparison
needs matching CPU count, memory size, runtime duration, and application
workload.

## Open The Offline Viewer

On the host:

```sh
cd apps/starry/resource-monitor/offline-viewer
python3 -m http.server 8000
```

Then open:

```text
http://127.0.0.1:8000/
```

The page can also be opened directly from the filesystem in browsers that allow
local file import. Import `system_metrics.csv`, optional `robot_trace.csv`, and
`events.jsonl` for StarryOS and/or Linux.

## Current StarryOS Metric Limits

The collector only reads interfaces that already exist in the running system. On
current StarryOS builds, some `/proc` counters may be absent or partial. The
following fields may therefore be `NA` or less meaningful than the Linux
baseline:

- per-core CPU fields when a guest exposes fewer than eight CPUs;
- page allocation/free counters;
- filesystem read/write counters;
- context-switch counters when `/proc/stat` does not expose real `ctxt` data;
- robot/AI timing fields unless the application provides `robot_trace.csv`.

Keeping these fields in the schema lets the viewer compare Linux and StarryOS
logs without inventing unavailable data, and leaves room for future app or kernel
instrumentation to fill them in.
