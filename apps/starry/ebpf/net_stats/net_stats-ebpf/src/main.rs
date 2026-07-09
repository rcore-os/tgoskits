#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]

// net_stats-ebpf: kprobe-based network statistics collector for ax-net.
//
// Probes the smoltcp phy layer in ax_net::router where all IP frames converge:
//   TxToken::consume  → TX packets/bytes
//   RxToken::consume  → RX packets/bytes
//
// TX byte length is read from the `len` scalar argument at entry.
// RX byte length is read from the `packet: &[u8]` slice field inside RxToken.
//
// Map layout (global array, index = NetKey enum value):
//   0  TX_PKTS    1  TX_BYTES
//   2  RX_PKTS    3  RX_BYTES

use aya_ebpf::{
    helpers::bpf_probe_read_kernel,
    macros::{kprobe, map},
    maps::Array,
    programs::ProbeContext,
};

const TX_PKTS: u32 = 0;
const TX_BYTES: u32 = 1;
const RX_PKTS: u32 = 2;
const RX_BYTES: u32 = 3;
const MAP_SIZE: u32 = 4;

#[map]
static NETSTATS: Array<u64> = Array::<u64>::with_max_entries(MAP_SIZE, 0);

#[inline(always)]
fn add_to(idx: u32, delta: u64) {
    if let Some(slot) = NETSTATS.get_ptr_mut(idx) {
        unsafe { *slot += delta };
    }
}

// TxToken::consume(self, len: usize, f: F) → on x86_64, len is in rsi (arg 1).
#[kprobe]
pub fn phy_tx(ctx: ProbeContext) -> u32 {
    if let Some(len) = ctx.arg::<usize>(1) {
        add_to(TX_PKTS, 1);
        add_to(TX_BYTES, len as u64);
    }
    0
}

// RxToken::consume(self, f: F) → self is RxToken at rdi (arg 0).
//
// struct RxToken {
//     interface_id: InterfaceId,
//     packet_meta: PacketMeta,
//     packet: &'a [u8],
// }
//
// RxToken::consume is heavily inlined into Interface::socket_ingress, making
// the actual memory layout at probe time differ from the source definition.
// The packet slice's length field offset within the inlined structure needs
// to be determined through runtime debugging or by tracing the actual packet
// access within the inlined code.
//
// For now, RX packet counts work correctly. RX byte counting is disabled
// until the correct offset is confirmed.
#[kprobe]
pub fn phy_rx(ctx: ProbeContext) -> u32 {
    add_to(RX_PKTS, 1);

    // TODO: Determine correct offset for packet.len within inlined RxToken.
    // Candidates tried: offset 48 (calculated), offset 16 (disasm hint).
    // Both produced unreasonable values or zeros with sanity check.
    //
    // Possible next steps:
    // 1. Use bpftrace to dump memory at self pointer
    // 2. Instrument the actual f(self.packet) call site
    // 3. Count RX bytes from an alternative probe point (e.g., driver layer)

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
