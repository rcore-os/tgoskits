# StarryOS Resource Monitor Viewer

This directory contains a small user-space helper for collecting and replaying
resource-monitoring data for StarryOS application experiments. It is intended
for offline board/QEMU debugging and demo evidence, not for cloud monitoring.

The feature has two parts:

- `system-monitor.sh`: a POSIX shell collector that samples existing `/proc`
  files and writes local CSV/JSONL logs.
- `offline-viewer/index.html`: a self-contained static HTML page that imports the
  logs with the browser File API and renders charts/tables without network
  access.

## Scope

This is an application-layer tool for StarryOS user-space experiments. It does
not add kernel counters, drivers, NPU tracing, USB tracing, robot control logic,
or online telemetry services. When a metric is not exposed by the current OS
interface, the collector writes `NA` instead of inventing a value.

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

Export these files to the host through serial logs, SD card, USB storage, TFTP,
rootfs copy, or a local QEMU shared directory.

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
local file import.

## StarryOS QEMU Smoke Check

A small QEMU test case is provided under:

```text
test-suit/starryos/qemu-smp1/offline-monitor/
```

It injects the collector into the StarryOS rootfs, runs it for 60 seconds, and
prints the generated logs between markers so the host can extract them.

```sh
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/offline-monitor
```

Use the extracted CSV/JSONL files as input to the offline viewer. On current
StarryOS branches, some `/proc` counters may be absent or partial; the collector
keeps those fields as `NA` and still validates the offline log path.

## Fair Comparisons

For a meaningful Linux vs StarryOS comparison, collect both datasets under the
same conditions: same QEMU memory size/core count or the same RK3588 board,
same sampling interval, same duration, and the same application workload. Host
Linux data should only be used for viewer smoke testing, not as a strict baseline
against StarryOS QEMU.
