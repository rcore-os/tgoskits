#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]

// netmon-ebpf: kprobe-based full-stack network performance monitor for the
// ax-net queue runtime. Layer overview (see README.md for details):
//
//   L3 protocol: count_tx / count_rx            — L2 frame counters, same
//                                                 source as /proc/net/dev
//   L2 queue:    port_tx / port_rx              — protocol-side frame latency
//   L1 schedule: sched_irq + queue_poll         — IRQ arrival to executor poll
//                                                 wake latency, poll duration
//   L0 SDIO:     sdio_read / sdio_write         — CMD53 DMA transfer latency
//   control:     wifi_start                     — WiFi control start latency
//
// All hooks are concrete non-generic functions carrying #[inline(never)] in
// the kernel so the symbols survive release-mode inlining. Latency probes
// record an entry timestamp in a shared slot and compute the age at the
// return probe; no return value is ever read, which sidesteps the sret ABI
// problem documented in the net_stats history.
//
// Per-frame duration probes are sampled (SAMPLE_MASK) to bound interpreted
// BPF overhead; counters always count every event.

use aya_ebpf::{
    helpers::bpf_ktime_get_ns,
    macros::{kprobe, kretprobe, map},
    maps::{Array, PerCpuArray},
    programs::{ProbeContext, RetProbeContext},
};
use netmon_common::*;

#[map]
static COUNTERS: PerCpuArray<u64> = PerCpuArray::<u64>::with_max_entries(CNT_SIZE, 0);
#[map]
static HISTS: PerCpuArray<u64> = PerCpuArray::<u64>::with_max_entries(HIST_SIZE, 0);
#[map]
static SAMPLE_CNT: PerCpuArray<u64> = PerCpuArray::<u64>::with_max_entries(1, 0);
#[map]
static TS_IRQ: Array<u64> = Array::<u64>::with_max_entries(1, 0);
#[map]
static TS_POLL: Array<u64> = Array::<u64>::with_max_entries(1, 0);
#[map]
static TS_PORT: Array<u64> = Array::<u64>::with_max_entries(1, 0);
#[map]
static TS_SDIO: Array<u64> = Array::<u64>::with_max_entries(1, 0);
#[map]
static TS_WIFI: Array<u64> = Array::<u64>::with_max_entries(1, 0);

/// Increment the per-CPU counter at `idx` by `delta`.
///
/// Always inlined so the compiled program has no function-call control flow.
#[inline(always)]
fn add_to(map: &PerCpuArray<u64>, idx: u32, delta: u64) {
    if let Some(slot) = map.get_ptr_mut(idx) {
        // SAFETY: PerCpuArray::get_ptr_mut returns a valid, properly aligned
        // pointer into the current CPU's private map slot. The slot is owned
        // exclusively by this CPU (BPF execution is non-preemptible), so no
        // concurrent modification is possible.
        unsafe { *slot += delta };
    }
}

/// Floor(log2(value)) clamped to `[0, H_BUCKETS)`. Bounded shift loop keeps
/// the compiled program free of unbounded control flow.
#[inline(always)]
fn bucket_of(value: u64) -> u32 {
    let mut bucket: u32 = 0;
    let mut remaining = value;
    while remaining > 1 && bucket < H_BUCKETS - 1 {
        remaining >>= 1;
        bucket += 1;
    }
    bucket
}

#[inline(always)]
fn hist_add(base: u32, value_ns: u64) {
    add_to(&HISTS, base + bucket_of(value_ns), 1);
}

/// Wall-clock nanoseconds from the BPF clock source.
#[inline(always)]
fn now_ns() -> u64 {
    // SAFETY: bpf_ktime_get_ns reads the kernel monotonic clock and is safe
    // to call from any BPF program context.
    unsafe { bpf_ktime_get_ns() }
}

/// Read-and-clear a shared timestamp slot, returning the elapsed nanoseconds
/// when the slot holds a fresh value. Returns `None` for empty, stale, or
/// deliberately skipped (`ts == 0`) entries.
#[inline(always)]
fn take_age(slot: &Array<u64>, max_age_ns: u64) -> Option<u64> {
    let ts = slot.get(0).copied().unwrap_or(0);
    if ts == 0 {
        return None;
    }
    let _ = slot.set(0, &0, 0);
    let age = now_ns().saturating_sub(ts);
    (age < max_age_ns).then_some(age)
}

/// Per-frame sampling decision: true every `SAMPLE_MASK + 1`-th call per CPU.
#[inline(always)]
fn sample_now() -> bool {
    let mut count = 0u64;
    if let Some(slot) = SAMPLE_CNT.get_ptr_mut(0) {
        // SAFETY: per-CPU private slot, same contract as `add_to`.
        unsafe {
            *slot = (*slot).wrapping_add(1);
            count = *slot;
        }
    }
    count & (SAMPLE_MASK as u64) == 0
}

// ---------------------------------------------------------------------------
// L3: DeviceHandle::count_tx / count_rx — the /proc/net/dev accounting points.
// ---------------------------------------------------------------------------

#[kprobe]
pub fn count_tx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(&COUNTERS, CNT_TX_PKTS, 1);
        add_to(&COUNTERS, CNT_TX_BYTES, len as u64);
    }
    0
}

#[kprobe]
pub fn count_rx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(&COUNTERS, CNT_RX_PKTS, 1);
        add_to(&COUNTERS, CNT_RX_BYTES, len as u64);
    }
    0
}

// ---------------------------------------------------------------------------
// L1: PollGroupState::schedule_irq + QueueGroupExecutor::poll.
// ---------------------------------------------------------------------------

#[kprobe]
pub fn sched_irq(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_IRQ, 1);
    let now = now_ns();
    let _ = TS_IRQ.set(0, &now, 0);
    0
}

#[kprobe]
pub fn queue_poll(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_POLL, 1);
    if let Some(age) = take_age(&TS_IRQ, IRQ_POLL_MAX_NS) {
        hist_add(HIST_IRQ_POLL, age);
    }
    let now = now_ns();
    let _ = TS_POLL.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn queue_poll_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_POLL, EVENT_MAX_NS) {
        hist_add(HIST_POLL_DUR, duration);
    }
    0
}

// ---------------------------------------------------------------------------
// L2: QueueFramePort::transmit / receive — protocol-side frame boundary.
// ---------------------------------------------------------------------------

#[kprobe]
pub fn port_tx(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_PORT_TX, 1);
    // Skipped frames store ts == 0 so the return probe can tell them apart.
    let now = if sample_now() { now_ns() } else { 0 };
    let _ = TS_PORT.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn port_tx_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_PORT, EVENT_MAX_NS) {
        hist_add(HIST_PORT_TX_DUR, duration);
    }
    0
}

#[kprobe]
pub fn port_rx(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_PORT_RX, 1);
    let now = if sample_now() { now_ns() } else { 0 };
    let _ = TS_PORT.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn port_rx_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_PORT, EVENT_MAX_NS) {
        hist_add(HIST_PORT_RX_DUR, duration);
    }
    0
}

// ---------------------------------------------------------------------------
// L0: SdioCard::submit_read_dma / submit_write_dma — CMD53 owned-DMA transfers.
// The owner executor serializes all SDIO traffic, so the shared timestamp
// slot is written and read without overlap.
// ---------------------------------------------------------------------------

#[kprobe]
pub fn sdio_read(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_SDIO_READ, 1);
    let now = now_ns();
    let _ = TS_SDIO.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn sdio_read_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_SDIO, EVENT_MAX_NS) {
        hist_add(HIST_SDIO_DUR, duration);
    }
    0
}

#[kprobe]
pub fn sdio_write(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_SDIO_WRITE, 1);
    let now = now_ns();
    let _ = TS_SDIO.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn sdio_write_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_SDIO, EVENT_MAX_NS) {
        hist_add(HIST_SDIO_DUR, duration);
    }
    0
}

// ---------------------------------------------------------------------------
// Control plane: WifiControl::start on AicWifiControl.
// ---------------------------------------------------------------------------

#[kprobe]
pub fn wifi_start(_ctx: ProbeContext) -> u32 {
    add_to(&COUNTERS, CNT_WIFI_START, 1);
    let now = now_ns();
    let _ = TS_WIFI.set(0, &now, 0);
    0
}

#[kretprobe]
pub fn wifi_start_ret(_ctx: RetProbeContext) -> u32 {
    if let Some(duration) = take_age(&TS_WIFI, EVENT_MAX_NS) {
        hist_add(HIST_WIFI_START_DUR, duration);
    }
    0
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // The eBPF verifier forbids infinite loops, so we use
    // unreachable_unchecked to tell LLVM to elide the landing pad
    // rather than emitting a loop {} that the verifier would reject.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
