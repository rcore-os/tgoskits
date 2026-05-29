# qperf Marker And Metrics Usage

`cargo starry perf` is now the preferred entrypoint for local qperf runs. The
lower-level harness commands below remain supported for Docker-wrapped syscall
harness workflows and compatibility.

```bash
cargo starry perf \
  --case blk-read \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
```

See also `docs/qperf-cargo-starry-integration.md` and
`docs/qperf-flamegraph-guide.md`.

## Basic Marker Run

Use explicit guest stdout markers around the workload:

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 60 \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 30 \
  --shell-init-cmd 'echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk-read'
```

The report window is written to `report.json.window` and `qperf/window.json`.

Important fields:

- `window.start_marker`
- `window.stop_marker`
- `window.start_time`
- `window.stop_time`
- `window.duration_sec`
- `window.truncated_by_timeout`
- `window.boot_samples_excluded`
- `window.warnings`

If the start marker is missing, the report includes a warning because boot samples may still be present. If the stop marker is missing, the window extends until QEMU exits or times out.

## Virtio Counter Run

The tools-side parser can merge any `QPERF_METRIC key=value` line printed by the
guest into `report.json.workload_metrics.values`. If the profiled StarryOS
branch provides `/proc/qperf_metrics`, enable parsing with `--qperf-metrics`,
reset before the workload, and print `/proc/qperf_metrics` before the stop
marker:

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 60 \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 30 \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
```

For net:

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 90 \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --shell-init-cmd 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:net-wget; wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; cat /proc/qperf_metrics; echo QPERF_END:net-wget'
```

When this command is run through the default Docker wrapper, the HTTP server for
`10.0.2.2:8000` must be inside the same container namespace as QEMU. One usable
pattern is to start `python3 -m http.server 8000 --bind 0.0.0.0` in the wrapper
before invoking `harness.py --no-docker`.

The parser merges `QPERF_METRIC key=value` fields into `report.json.workload_metrics.values`.
This tools-only integration does not create `/proc/qperf_metrics` by itself;
that procfs export is a separate kernel/driver instrumentation concern.

## Counter Interpretation

When a profiled branch exports counters through `/proc/qperf_metrics`, those
counters are driver-visible observations. They are useful for A/B validation,
but they should not be described as exact virtqueue ring-level accounting.

Important blk counters:

- `virtqueue_add_notify_wait_pop_count`: synchronous virtqueue submit/notify/wait/pop path count.
- `virtqueue_add_count`: driver-visible virtqueue add count.
- `virtio_notify_kick_count`: driver-visible notify/kick count.
- `virtqueue_pop_complete_count`: driver-visible completion pop count.
- `virtqueue_depth_max` and `virtqueue_depth_hist_*`: approximate queue depth observation.
- `virtio_blk_read_requests` and `virtio_blk_read_bytes`: blk read request count and bytes recorded by the driver.

One existing marker-aware blk profile at
`target/qperf-validation/blk/perf/riscv64/latest/report.json` reported:

| metric | value |
| --- | ---: |
| `dd` bytes | 53,601,104 |
| `dd` elapsed | 5.794463 s |
| `virtqueue_add_notify_wait_pop_count` | 13,780 |
| `virtio_notify_kick_count` | 13,847 |
| `virtio_blk_read_requests` | 13,478 |
| `virtio_blk_read_bytes` | 55,195,136 |
| average blk read request size | 4,095.20 bytes |

This is a useful bottleneck signal: even when the guest command uses
`bs=64k`, the driver-visible blk request size is still about 4 KiB and the
number of notify/kick events is close to the number of virtqueue adds. See
`docs/qperf-current-blk-bottleneck-analysis.md` for the full analysis.

## Host Perf

Host perf is opt-in:

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --host-time \
  --host-perf \
  --host-perf-events task-clock,cycles,instructions,cache-references,cache-misses
```

These counters measure the host QEMU process. They are not guest PMU counters.

## A/B Compare

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline/report.json \
  --candidate target/starry-syscall-harness/perf/riscv64/candidate/report.json \
  --name blk-read-ab
```

The comparison output is under:

```text
target/starry-syscall-harness/perf-compare/blk-read-ab/
```

`compare.md` gives an automatic conclusion: `明显改善`, `基本无变化`, `退化`, or `数据不足`.

## Limitations

- Runtime qperf plugin enable/disable is not implemented; filtering is done by sample timestamps during analyzer postprocess.
- Driver-visible queue depth and notify/kick counters are approximate. Exact ring-level accounting requires adding counters inside `virtio-drivers`.
- Place `cat /proc/qperf_metrics` before the stop marker. The harness asks QEMU to quit once the stop marker is observed.
- If `/dev/vhost-vsock` is missing on the host, vsock experiments should record the environment blocker and must not fabricate vsock throughput or counters.
