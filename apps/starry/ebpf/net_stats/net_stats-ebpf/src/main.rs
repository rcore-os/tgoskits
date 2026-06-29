#![no_std]
#![no_main]

// net_stats-ebpf: kprobe-based network statistics collector for ax-net.
//
// Probes four symbols in the Starry kernel:
//   ax_net::tcp::TcpSocket::send   → TCP tx bytes/packets
//   ax_net::tcp::TcpSocket::recv   → TCP rx bytes/packets
//   ax_net::udp::UdpSocket::send   → UDP tx bytes/packets
//   ax_net::udp::UdpSocket::recv   → UDP rx bytes/packets
//
// Because the send/recv functions return the number of bytes transferred
// (wrapped in AxResult<usize>), we probe the *return* site with kretprobe
// for the byte count and use separate kprobe entry counters for packets.
// Both directions parse the same `AxResult<usize>` sret-pointer layout, so
// the probe does not depend on architecture-specific register conventions.
//
// Map layout (global array, index = NetKey enum value):
//   0  TCP_TX_PKTS   1  TCP_TX_BYTES
//   2  TCP_RX_PKTS   3  TCP_RX_BYTES
//   4  UDP_TX_PKTS   5  UDP_TX_BYTES
//   6  UDP_RX_PKTS   7  UDP_RX_BYTES

use aya_ebpf::{
    macros::{kprobe, kretprobe, map},
    maps::Array,
    programs::{ProbeContext, RetProbeContext},
};

const TCP_TX_PKTS: u32 = 0;
const TCP_TX_BYTES: u32 = 1;
const TCP_RX_PKTS: u32 = 2;
const TCP_RX_BYTES: u32 = 3;
const UDP_TX_PKTS: u32 = 4;
const UDP_TX_BYTES: u32 = 5;
const UDP_RX_PKTS: u32 = 6;
const UDP_RX_BYTES: u32 = 7;
const MAP_SIZE: u32 = 8;
const MAX_IO_BYTES: u64 = 1 << 30;

#[map]
static NETSTATS: Array<u64> = Array::<u64>::with_max_entries(MAP_SIZE, 0);

#[inline(always)]
fn add_to(idx: u32, delta: u64) {
    if let Some(slot) = NETSTATS.get_ptr_mut(idx) {
        unsafe { *slot += delta };
    }
}

// Both `send` and `recv` in `ax_net::socket::SocketOps` share the same
// signature shape, returning `AxResult<usize>` (i.e. `Result<usize, AxError>`
// where `AxError` wraps an `i32`). That value is larger than a single register,
// so the ABI returns it through an sret pointer; at the kretprobe site the
// return register holds that pointer. The in-memory layout is:
//   [+0]  u64  discriminant  (0 = Ok, non-zero = Err)
//   [+8]  u64  payload       (byte count on Ok)
//
// Using this layout for both send and recv avoids depending on architecture-
// specific argument-register conventions, so the probe stays correct across
// compiler versions, optimization levels, and target architectures.
#[inline(always)]
fn read_ok_bytes_from_ptr(ptr: u64) -> Option<u64> {
    let ptr = ptr as *const u64;
    if ptr.is_null() {
        return None;
    }
    // discriminant at offset 0
    let disc = unsafe { aya_ebpf::helpers::bpf_probe_read_kernel(ptr).ok()? };
    if disc != 0u64 {
        return None; // Err variant
    }
    // payload at offset 8
    let bytes = unsafe { aya_ebpf::helpers::bpf_probe_read_kernel(ptr.add(1)).ok()? };
    if bytes <= MAX_IO_BYTES {
        Some(bytes)
    } else {
        None
    }
}

// ── TCP send (entry → count packet; retprobe → count bytes) ─────────────────

#[kprobe]
pub fn tcp_send_entry(_ctx: ProbeContext) -> u32 {
    add_to(TCP_TX_PKTS, 1);
    0
}

#[kretprobe]
pub fn tcp_send_ret(ctx: RetProbeContext) -> u32 {
    if let Some(n) = read_ok_bytes_from_ptr(ctx.ret::<u64>()) {
        add_to(TCP_TX_BYTES, n);
    }
    0
}

// ── TCP recv ─────────────────────────────────────────────────────────────────

#[kprobe]
pub fn tcp_recv_entry(_ctx: ProbeContext) -> u32 {
    add_to(TCP_RX_PKTS, 1);
    0
}

#[kretprobe]
pub fn tcp_recv_ret(ctx: RetProbeContext) -> u32 {
    if let Some(n) = read_ok_bytes_from_ptr(ctx.ret::<u64>()) {
        add_to(TCP_RX_BYTES, n);
    }
    0
}

// ── UDP send ─────────────────────────────────────────────────────────────────

#[kprobe]
pub fn udp_send_entry(_ctx: ProbeContext) -> u32 {
    add_to(UDP_TX_PKTS, 1);
    0
}

#[kretprobe]
pub fn udp_send_ret(ctx: RetProbeContext) -> u32 {
    if let Some(n) = read_ok_bytes_from_ptr(ctx.ret::<u64>()) {
        add_to(UDP_TX_BYTES, n);
    }
    0
}

// ── UDP recv ─────────────────────────────────────────────────────────────────

#[kprobe]
pub fn udp_recv_entry(_ctx: ProbeContext) -> u32 {
    add_to(UDP_RX_PKTS, 1);
    0
}

#[kretprobe]
pub fn udp_recv_ret(ctx: RetProbeContext) -> u32 {
    if let Some(n) = read_ok_bytes_from_ptr(ctx.ret::<u64>()) {
        add_to(UDP_RX_BYTES, n);
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
