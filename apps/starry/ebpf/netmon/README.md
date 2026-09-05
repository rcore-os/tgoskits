# netmon - Full-Stack Network Performance eBPF Monitor

Full-stack network performance monitoring for the StarryOS ax-net queue
runtime using eBPF kprobes. Covers the path from the SDIO device transfer up
through the queue runtime and the L2 protocol boundary.

## Probed layers

| Layer | Probes | Metrics |
| --- | --- | --- |
| L3 protocol | `DeviceHandle::count_tx` / `count_rx` | L2 frame packet/byte counters (same accounting points as `/proc/net/dev`) |
| L2 queue | `QueueFramePort::transmit` / `receive` | Protocol-side frame count and per-frame duration (sampled) |
| L1 schedule | `PollGroupState::schedule_irq` + `QueueGroupExecutor::poll` | IRQ count, IRQ-to-poll wake latency, poll cycle duration |
| L0 SDIO | `SdioCard::submit_read_dma` / `submit_write_dma` | CMD53 DMA transfer count and duration |
| control | `AicWifiControl::start` (`WifiControl`) | WiFi control start count and duration |

All hooks are concrete non-generic kernel functions carrying
`#[inline(never)]` so they survive release-mode inlining. Every probe must
resolve to exactly one `/proc/kallsyms` symbol; the loader fails otherwise,
guarding against generic-monomorphization double counting. SDIO and WiFi
probes are optional: when their driver is not built into the running image
(e.g. virtio-only QEMU builds), the loader skips them with a warning.

Latency is measured as the age of a shared entry timestamp read at the
return probe. Return values are never read, which sidesteps the sret ABI
limitation documented in the net_stats history.

Per-frame duration probes sample every 4th frame (`SAMPLE_MASK = 3`) to
bound interpreted-BPF overhead; counters always count every event.

## Usage

### Interactive mode (default)

Periodic snapshots every N seconds (default 5):

```sh
/usr/bin/netmon --interval 1
```

### Snapshot mode

Print one snapshot and exit (for scripted sampling):

```sh
/usr/bin/netmon --once
```

### Test mode

Attach probes, run loopback TCP/UDP traffic, and assert the L3 counters move:

```sh
/usr/bin/netmon --test
```

Loopback frames take the Router fast path and bypass `QueueFramePort`, so
`--test` only asserts the L3 counters. Queue, SDIO, and WiFi probes require
real device traffic and are validated on the board.

## Output format

Snapshots are wrapped in `NETMON_BEGIN` / `NETMON_END` markers:

```
NETMON_BEGIN
tx_pkts=2 tx_bytes=148 rx_pkts=2 rx_bytes=148
irq=0 poll=3 port_tx=0 port_rx=0 sdio_read=0 sdio_write=0 wifi_start=0
hist_irq_poll=0,0,0,...
hist_poll_dur=...
hist_port_tx_dur=...
hist_port_rx_dur=...
hist_sdio_dur=...
hist_wifi_start_dur=...
NETMON_END
```

Histogram bucket `i` covers `[2^i, 2^(i+1))` nanoseconds; the last bucket
(31) clamps every larger value. Bucket counts are summed across CPUs.

## Build & run

```sh
cargo xtask starry app qemu -t ebpf/netmon --arch x86_64
```

`prebuild.sh` cross-builds the loader as a static musl binary and installs it
into the rootfs overlay as `/usr/bin/netmon`. x86_64 is validated; aarch64,
riscv64, and loongarch64 builds follow the same layout and are pending
runtime validation.

## Relationship to net_stats and /proc/net/queue

- `net_stats` counts L2 frames only. `netmon` extends the same kprobe
  technique across all four layers and adds latency histograms.
- The queue-runtime counters exposed by `NetQueueStats` are also available
  from `/proc/net/queue` without any eBPF attachment; the kernel-side
  accounting is the authoritative source for irq/schedule/missed/budget
  counters, while netmon measures their timing distribution.
