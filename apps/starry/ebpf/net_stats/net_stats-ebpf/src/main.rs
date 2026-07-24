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
use net_stats_common::{MAP_SIZE, RX_BYTES, RX_PKTS, TX_BYTES, TX_PKTS};

#[map]
static NETSTATS: PerCpuArray<u64> = PerCpuArray::<u64>::with_max_entries(MAP_SIZE, 0);

/// Increment the counter at `idx` by `delta`.
///
/// With `PerCpuArray`, `get_ptr_mut(idx)` returns a pointer into the
/// *current CPU's* private slot, so the increment is naturally race-free.
///
/// Always inlined so the eBPF verifier can trivially prove bounded
/// execution and won't reject the program for function calls.
#[inline(always)]
fn add_to(idx: u32, delta: u64) {
    if let Some(slot) = NETSTATS.get_ptr_mut(idx) {
        // SAFETY: PerCpuArray::get_ptr_mut returns a valid, properly
        // aligned pointer into the current CPU's private map slot.
        // The slot is owned exclusively by this CPU (BPF execution is
        // non-preemptible), so no concurrent modification is possible.
        unsafe { *slot += delta };
    }
}

// count_tx(&self, len: usize)
//   The probe reads len from arg 1. aya's ProbeContext handles
//   per-architecture register mapping (rdi/rsi on x86_64, x0/x1 on
//   aarch64, a0/a1 on riscv64, a0/a1 on loongarch64).
#[kprobe]
pub fn count_tx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(TX_PKTS, 1);
        add_to(TX_BYTES, len as u64);
    }
    0
}

// count_rx(&self, len: usize)
//   Same ABI as count_tx — len is always arg 1 regardless of architecture.
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
    // The eBPF verifier forbids infinite loops, so we use
    // unreachable_unchecked to tell LLVM to elide the landing pad
    // rather than emitting a loop {} that the verifier would reject.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
