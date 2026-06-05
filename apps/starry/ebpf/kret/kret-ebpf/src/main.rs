#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{kretprobe, map},
    maps::HashMap,
    programs::RetProbeContext,
};

// kret: a kretprobe on `sys_getpid`. We only need to prove the *return* probe
// fires, so each hit bumps a single HashMap counter the loader reads back.
// (aya_log uses a perf-event ringbuf StarryOS does not surface to userspace,
// so logging is replaced by this HashMap, matching the other demos.)
#[map]
static KRET_HITS: HashMap<u32, u64> = HashMap::<u32, u64>::with_max_entries(4, 0);

const HIT_KEY: u32 = 0;

#[kretprobe]
pub fn kret(_ctx: RetProbeContext) -> u32 {
    let new_v = unsafe { KRET_HITS.get(HIT_KEY).map(|v| *v + 1).unwrap_or(1) };
    let _ = KRET_HITS.insert(HIT_KEY, new_v, 0);
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
