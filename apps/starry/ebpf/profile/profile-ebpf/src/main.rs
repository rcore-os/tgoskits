#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::HashMap,
    programs::TracePointContext,
};
use profile_common::SYSCALL_ID_OFFSET;

// Histogram: syscall number -> hit count. A plain BPF_MAP_TYPE_HASH (no
// ringbuf / mmap dependency), iterated and ranked by the userspace loader.
// 1024 entries comfortably covers the whole Linux syscall number space.
#[map]
static SYSCALL_HIST: HashMap<u32, u64> = HashMap::<u32, u64>::with_max_entries(1024, 0);

// Use Linux's `raw_syscalls:sys_enter` ABI to build a frequency profile across
// the whole syscall surface. The cooked payload is stable across Starry's
// 64-bit architectures and does not depend on registers or a mangled kernel
// symbol remaining probeable.
#[tracepoint]
pub fn profile(ctx: TracePointContext) -> u32 {
    try_profile(&ctx).unwrap_or(0)
}

fn try_profile(ctx: &TracePointContext) -> Result<u32, u32> {
    let sysno = unsafe { ctx.read_at::<i64>(SYSCALL_ID_OFFSET) }.map_err(|_| 1u32)?;
    if sysno < 0 {
        return Ok(0);
    }
    let sysno = sysno as u32;

    // map[sysno] += 1. The verifier rejects loops; this is straight-line.
    let next = unsafe { SYSCALL_HIST.get(sysno) }
        .map(|v| *v + 1)
        .unwrap_or(1);
    let _ = SYSCALL_HIST.insert(sysno, next, 0);
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // The verifier rejects loops, so a spinning handler would be rejected at
    // load time; mark it unreachable as the other in-tree programs do.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
