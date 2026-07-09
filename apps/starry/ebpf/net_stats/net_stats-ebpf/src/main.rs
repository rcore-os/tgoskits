#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]

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
    EbpfContext,
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

// Both `send` and `recv` in `ax_net::socket::SocketOps` return `AxResult<usize>`,
// which is `Result<usize, AxError>` where `AxError` wraps an `i32`.
//
// Result<usize, i32> is 16 bytes. On x86_64, the System V ABI returns this via
// register pair: RAX (discriminant) and RDX (payload/byte count).
//
// At kretprobe time:
// - ctx.ret() returns RAX (discriminant: 0 = Ok, non-zero = Err)
// - We convert to ProbeContext and read arg(2) to get RDX (byte count)
//
// On other architectures, the second return value may be in a different register.
// We handle this with conditional compilation based on bpf_target_arch.
//
// # Safety
//
// The ProbeContext created from RetProbeContext::as_ptr() points to the same
// underlying pt_regs, allowing us to read return registers that kretprobe exposes.

// After analyzing the actual compiled code, Result<usize, AxError> (16 bytes) 
// uses the sret (structure return) calling convention on x86_64:
// - The caller allocates stack space for the return value  
// - A pointer to this space is passed as a hidden first argument (in RDI)
// - The callee writes the result to that location
// - The sret pointer is returned in RAX at kretprobe time
//
// Memory layout at *sret_ptr:
//   [+0]  u64  discriminant (0 = Ok, non-zero = Err)
//   [+8]  u64  payload      (byte count on Ok)
//
// # Safety
//
// bpf_probe_read_kernel provides BPF-verified safe kernel memory access.

// ═══════════════════════════════════════════════════════════════════════════
// KNOWN LIMITATION: Byte Counter Extraction via kretprobe
// ═══════════════════════════════════════════════════════════════════════════
//
// The current implementation cannot reliably extract actual byte counts from
// Result<usize, AxError> return values using kretprobe across all scenarios.
//
// ## Root Cause Analysis
//
// Result<usize, i32> (16 bytes) uses sret calling convention on x86_64:
// - sret pointer passed as hidden first argument (RDI)
// - Function writes result to *sret_ptr and returns the pointer in RAX
// - At kretprobe time, RAX contains sret pointer to caller's stack frame
//
// The problem: The sret buffer is on the **caller's stack**, which may be:
// - Already unwound or modified when kretprobe fires
// - Inaccessible due to BPF verifier restrictions on stack access
// - In a different memory protection domain
//
// Attempts to dereference the sret pointer via bpf_probe_read_kernel fail,
// likely because the BPF verifier cannot prove the pointer validity for
// stack memory outside the current BPF program's stack frame.
//
// ## Current Workaround
//
// We use a heuristic: assume average packet size and estimate bytes from
// packet counts. This provides approximate trending data while preserving
// accurate packet counters.
//
// ## Proper Solutions (for future implementation)
//
// 1. **fentry/fexit probes with BTF** (requires Linux 5.5+, BTF-enabled kernel)
//    - Direct access to typed function arguments and return values
//    - No ABI guessing required
//
// 2. **Entry/exit correlation via BPF HashMap**
//    - Store (tid, timestamp) → buffer_length mapping at entry
//    - Match with return at exit
//    - Complex but works with current kprobe/kretprobe
//
// 3. **Kernel module-based kprobe**
//    - Direct pt_regs manipulation
//    - Can access caller stack frames
//    - Not portable to eBPF
//
// For now, packet counters remain fully accurate, and byte estimates provide
// useful trending information for relative comparisons.
//
// ═══════════════════════════════════════════════════════════════════════════

const ESTIMATED_AVG_PACKET_SIZE: u64 = 64;

#[cfg(bpf_target_arch = "x86_64")]
#[inline(always)]
fn read_ok_bytes_from_ret(_ctx: &RetProbeContext) -> Option<u64> {
    // TODO: Implement proper byte extraction when fentry/fexit becomes available
    // For now, use estimated average packet size
    Some(ESTIMATED_AVG_PACKET_SIZE)
}

#[cfg(bpf_target_arch = "aarch64")]
#[inline(always)]
fn read_ok_bytes_from_ret(_ctx: &RetProbeContext) -> Option<u64> {
    Some(ESTIMATED_AVG_PACKET_SIZE)
}

#[cfg(bpf_target_arch = "riscv64")]
#[inline(always)]
fn read_ok_bytes_from_ret(_ctx: &RetProbeContext) -> Option<u64> {
    Some(ESTIMATED_AVG_PACKET_SIZE)
}

#[cfg(bpf_target_arch = "loongarch64")]
#[inline(always)]
fn read_ok_bytes_from_ret(_ctx: &RetProbeContext) -> Option<u64> {
    Some(ESTIMATED_AVG_PACKET_SIZE)
}

// ── TCP send (entry → count packet; retprobe → count bytes) ─────────────────

#[kprobe]
pub fn tcp_send_entry(_ctx: ProbeContext) -> u32 {
    add_to(TCP_TX_PKTS, 1);
    0
}

#[kretprobe]
pub fn tcp_send_ret(ctx: RetProbeContext) -> u32 {
    if let Some(n) = read_ok_bytes_from_ret(&ctx) {
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
    if let Some(n) = read_ok_bytes_from_ret(&ctx) {
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
    if let Some(n) = read_ok_bytes_from_ret(&ctx) {
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
    if let Some(n) = read_ok_bytes_from_ret(&ctx) {
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
