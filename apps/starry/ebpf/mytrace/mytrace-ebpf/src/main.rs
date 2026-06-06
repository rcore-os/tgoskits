#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::HashMap,
    programs::TracePointContext,
};

// mytrace: a cooked tracepoint on `syscalls:sys_enter_openat`. We only need to
// prove the tracepoint fires and delivers its context, so each hit bumps a
// single HashMap counter the loader reads back. (aya_log uses a perf-event
// ringbuf StarryOS does not surface to userspace, so logging is replaced by
// this HashMap, matching the other demos.)
#[map]
static OPENAT_HITS: HashMap<u32, u64> = HashMap::<u32, u64>::with_max_entries(4, 0);

const HIT_KEY: u32 = 0;

#[tracepoint]
pub fn mytrace(ctx: TracePointContext) -> u32 {
    // Touch the context to prove it is readable: the StarryOS sys_enter_openat
    // record carries `path` (the filename pointer) at byte offset 16 (8-byte
    // common header + dfd:i32 + o_flags:u32). Reading it must not fault; the
    // value itself is not asserted, only that the probe fired (the count).
    let _path: u64 = unsafe { ctx.read_at(16).unwrap_or(0) };
    let new_v = unsafe { OPENAT_HITS.get(HIT_KEY).map(|v| *v + 1).unwrap_or(1) };
    let _ = OPENAT_HITS.insert(HIT_KEY, new_v, 0);
    0
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // The verifier rejects loops, so mark unreachable like the other programs.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
