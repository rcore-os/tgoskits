#![no_std]

// Shared constants for the netmon eBPF program and userspace loader.
// Both crates depend on netmon-common so map slot indices stay in sync.

/// Counter index: transmitted packets (L2 frames counted by `count_tx`).
pub const CNT_TX_PKTS: u32 = 0;
/// Counter index: transmitted bytes (L2 frame length).
pub const CNT_TX_BYTES: u32 = 1;
/// Counter index: received packets (L2 frames counted by `count_rx`).
pub const CNT_RX_PKTS: u32 = 2;
/// Counter index: received bytes (L2 frame length).
pub const CNT_RX_BYTES: u32 = 3;
/// Counter index: hard-IRQ schedule events (`PollGroupState::schedule_irq`).
pub const CNT_IRQ: u32 = 4;
/// Counter index: queue executor poll cycles (`QueueGroupExecutor::poll`).
pub const CNT_POLL: u32 = 5;
/// Counter index: protocol-side TX frames (`QueueFramePort::transmit`).
pub const CNT_PORT_TX: u32 = 6;
/// Counter index: protocol-side RX frames (`QueueFramePort::receive`).
pub const CNT_PORT_RX: u32 = 7;
/// Counter index: SDIO CMD53 DMA reads (`SdioCard::submit_read_dma`).
pub const CNT_SDIO_READ: u32 = 8;
/// Counter index: SDIO CMD53 DMA writes (`SdioCard::submit_write_dma`).
pub const CNT_SDIO_WRITE: u32 = 9;
/// Counter index: WiFi control start requests (`AicWifiControl::start`).
pub const CNT_WIFI_START: u32 = 10;
/// Total number of counter slots in the `COUNTERS` BPF map.
pub const CNT_SIZE: u32 = 11;

/// Number of log2 histogram buckets: bucket `i` covers `[2^i, 2^(i+1))`
/// nanoseconds for `i < H_BUCKETS - 1`; the last bucket clamps larger values.
pub const H_BUCKETS: u32 = 32;
/// Histogram base slot: IRQ arrival to queue poll entry latency.
pub const HIST_IRQ_POLL: u32 = 0;
/// Histogram base slot: queue executor poll cycle duration.
pub const HIST_POLL_DUR: u32 = 32;
/// Histogram base slot: protocol-side TX frame duration.
pub const HIST_PORT_TX_DUR: u32 = 64;
/// Histogram base slot: protocol-side RX frame duration.
pub const HIST_PORT_RX_DUR: u32 = 96;
/// Histogram base slot: SDIO CMD53 DMA transfer duration.
pub const HIST_SDIO_DUR: u32 = 128;
/// Histogram base slot: WiFi control start duration.
pub const HIST_WIFI_START_DUR: u32 = 160;
/// Total number of histogram slots in the `HISTS` BPF map.
pub const HIST_SIZE: u32 = 192;

/// Per-frame duration probes measure every `SAMPLE_MASK + 1`-th frame to keep
/// interpreted-BPF overhead bounded; counter probes always measure all frames.
pub const SAMPLE_MASK: u32 = 3;

/// Acceptance window for the IRQ-to-poll latency pairing. The IRQ timestamp
/// slot is written on the IRQ CPU and read on the owner CPU, so a value older
/// than this window is treated as stale and dropped.
pub const IRQ_POLL_MAX_NS: u64 = 1 << 27;

/// Acceptance window for same-CPU entry/return duration pairings. Values
/// beyond this are treated as stale slots rather than real durations.
pub const EVENT_MAX_NS: u64 = 1 << 31;
