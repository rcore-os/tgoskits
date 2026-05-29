# qperf Tooling Redesign

## Goals

This redesign turns qperf from a boot-to-exit stack sampler into a workload-oriented experiment tool for virtio-blk, virtio-net, and virtio-vsock attribution.

The main changes are:

- workload windows driven by guest stdout markers;
- timestamped qperf samples and analyzer-side time filtering;
- hotspot category aggregation in addition to symbol hotspots;
- workload stdout metric parsing for `dd`, `wget`, and `QPERF_METRIC`;
- feature-gated virtio counters exported through `/proc/qperf_metrics`;
- report-level A/B comparison for baseline and candidate runs.

## Sampling Window

`cargo xtask starry perf` and `harness.py perf-profile` accept:

- `--start-marker TEXT`
- `--stop-marker TEXT`
- `--workload-timeout SECONDS`

When a start marker is observed in guest stdout, the host records elapsed time since QEMU launch. When a stop marker is observed, qperf asks QEMU to quit through QMP, falling back to SIGINT if QMP is unavailable.

For shell-injected workloads, the monitor disables shell echo before sending the command so marker matching is driven by workload output rather than by the echoed command line.

The qperf plugin now writes raw records as format version 2:

- `elapsed_ns`
- stack IP trace

`qperf-analyzer resolve` accepts `--start-sec`, `--stop-sec`, and `--stats`. The generated folded stack and flamegraph are filtered to the marker window when timestamps are available. Older raw files are still accepted, but elapsed-time filtering cannot be applied to format version 1 samples.

## Attribution Categories

The harness parses `qperf/stack.folded` and writes inclusive category totals to:

- `hotspot_categories.csv`
- `report.json.hotspots.category_totals`
- `report.md`

The current category set is:

- `virtqueue_add_notify_wait_pop`
- `virtqueue_add`
- `virtqueue_pop_complete`
- `virtio_notify_kick`
- `memcpy`
- `memmove`
- `allocator`
- `scheduler_wait_preempt`
- `lock_mutex_wait`
- `pci_probe_transport`
- `net_inflight_btree`
- `block_io_path`
- `net_rx_tx_path`
- `vsock_tx_rx_path`

Categories are inclusive and non-exclusive: one stack can contribute to both a subsystem category, such as `net_rx_tx_path`, and a bottleneck category, such as `memmove`.

## Workload Metrics

The harness parses guest stdout for:

- `dd` byte count, elapsed seconds, and throughput;
- `wget` length/saved byte count and elapsed seconds when visible;
- custom `QPERF_METRIC key=value` fields.

Parsed values are stored in:

- `report.json.workload_metrics`
- `report.json.normalized_metrics`

Normalized fields include:

- `guest_instructions_per_MB`
- `guest_blocks_per_MB`
- `host_elapsed_sec_per_MB`
- `samples_per_MB`
- `category_samples_per_MB`

Host perf stat output is included only when `--host-perf` is enabled. Otherwise the report explicitly records `未启用 host perf`.

## Virtio Counters

The `qperf-metrics` feature is off by default. When enabled, `ax-driver` records lightweight `AtomicU64` counters in the virtio glue layer:

- blk read/write request count and bytes;
- net RX/TX packet count and bytes;
- net RX `copy_within` count and bytes;
- net TX staging copy count and bytes;
- inflight map insert/remove/get count;
- approximate virtqueue add/notify/pop counts from driver submit/reclaim points;
- approximate queue depth max and histogram from driver-visible inflight depth.

The counters are exported by StarryOS at `/proc/qperf_metrics` as `QPERF_METRIC` lines. Writing `reset` clears the counters.

These counters are intentionally described as driver-visible approximations. Exact descriptor-ring depth, exact notify/kick count, and `VirtQueue::add_notify_wait_pop()` internals require instrumentation inside the `virtio-drivers` crate.

## A/B Compare

Use:

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline \
  --candidate target/starry-syscall-harness/perf/riscv64/candidate
```

Inputs can be a `report.json`, a profile directory, a qperf directory, or a folded stack file. Outputs are:

- `compare.json`
- `compare.md`
- `compare.csv`

The comparison includes workload throughput/elapsed time, guest executed instructions/blocks, host time/perf metrics, hotspot categories, virtio counters, copy bytes, notify/kick count, and queue depth fields when present. Missing fields are rendered as `N/A` instead of failing.
