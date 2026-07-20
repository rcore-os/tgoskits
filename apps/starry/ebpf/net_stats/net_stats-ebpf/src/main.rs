#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]

// net_stats-ebpf: kprobe-based network statistics collector for ax-net.
//
// Probes the exact functions where ax-net maintains the `/proc/net/dev`
// counters (DeviceHandle::count_rx / DeviceHandle::count_tx), so the
// eBPF counters stay consistent with the kernel's own accounting.
//
//   count_tx(&self, len: usize)  → TX_PKTS +1, TX_BYTES +len
//   count_rx(&self, len: usize)  → RX_PKTS +1, RX_BYTES +len
//
// Both functions must carry #[inline(never)] in router.rs so that they
// survive LLVM inlining and remain attachable by kprobe.
//
// Map layout (per-CPU array, index = counter slot):
//   0  TX_PKTS    1  TX_BYTES
//   2  RX_PKTS    3  RX_BYTES
//
// Per-CPU slots avoid cache-line contention: each CPU writes its own
// slot, and the userspace loader sums across CPUs when reading.

use aya_ebpf::{
    macros::{kprobe, map},
    maps::PerCpuArray,
    programs::ProbeContext,
};

const TX_PKTS: u32 = 0;
const TX_BYTES: u32 = 1;
const RX_PKTS: u32 = 2;
const RX_BYTES: u32 = 3;
const MAP_SIZE: u32 = 4;

#[map]
static NETSTATS: PerCpuArray<u64> = PerCpuArray::<u64>::with_max_entries(MAP_SIZE, 0);

/// Increment the counter at `idx` by `delta`.
///
/// With `PerCpuArray`, `get_ptr_mut(idx)` returns a pointer into the
/// *current CPU's* private slot, so the increment is naturally race-free.
#[inline(always)]
fn add_to(idx: u32, delta: u64) {
    if let Some(slot) = NETSTATS.get_ptr_mut(idx) {
        unsafe { *slot += delta };
    }
}

// count_tx(&self, len: usize)
//   x86_64: &self in rdi (arg 0), len in rsi (arg 1)
#[kprobe]
pub fn count_tx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(TX_PKTS, 1);
        add_to(TX_BYTES, len as u64);
    }
    0
}

// count_rx(&self, len: usize)
//   x86_64: &self in rdi (arg 0), len in rsi (arg 1)
#[kprobe]
pub fn count_rx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(RX_PKTS, 1);
        add_to(RX_BYTES, len as u64);
    }
    0
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
