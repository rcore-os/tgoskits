#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, uprobe},
    maps::HashMap,
    programs::ProbeContext,
};

// StarryOS note: aya_log writes through a perf-event ringbuf that StarryOS's
// BpfPerfEvent path does not surface to userspace (the same limitation
// documented in the syscall_count demo). So instead of logging, the uprobe
// records hits keyed by the probed function's first argument into a HashMap
// the loader reads back — a deterministic signal that the uprobe fired.
#[map]
static UPROBE_HITS: HashMap<u32, u64> = HashMap::<u32, u64>::with_max_entries(16, 0);

#[uprobe]
pub fn upb(ctx: ProbeContext) -> u32 {
    try_upb(ctx).unwrap_or_else(|ret| ret)
}

fn try_upb(ctx: ProbeContext) -> Result<u32, u32> {
    // First argument of the probed `uprobe_test(a: u32, ...)`.
    let arg: u32 = ctx.arg(0).ok_or(1u32)?;
    unsafe {
        let new_v = UPROBE_HITS.get(arg).map(|v| *v + 1).unwrap_or(1);
        let _ = UPROBE_HITS.insert(arg, new_v, 0);
    }
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // The verifier forbids loops, so use an unreachable hint instead.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
