#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{kprobe, map},
    maps::HashMap,
    programs::ProbeContext,
};

// Histogram: syscall number -> hit count. A plain BPF_MAP_TYPE_HASH (no
// ringbuf / mmap dependency), iterated and ranked by the userspace loader.
// 1024 entries comfortably covers the whole Linux syscall number space.
#[map]
static SYSCALL_HIST: HashMap<u32, u64> = HashMap::<u32, u64>::with_max_entries(1024, 0);

// D3 `profile`: kprobe on `starry_kernel::syscall::sysno(id: usize)`, the
// `#[inline(never)]` helper `handle_syscall` calls once per syscall with the
// raw syscall number as its first argument. Unlike D1 (which exact-counts one
// specific probed syscall), this builds a *frequency profile* across the whole
// syscall surface — a "perf top" for syscalls — reusing only the proven kprobe
// + HashMap path (no perf ringbuf, no smp_processor_id/pid helpers, which
// StarryOS does not register). Reading the number straight off `ctx.arg(0)`
// (rather than dereferencing a `&UserContext`) keeps it arch-independent.
#[kprobe]
pub fn profile(ctx: ProbeContext) -> u32 {
    try_profile(&ctx).unwrap_or(0)
}

fn try_profile(ctx: &ProbeContext) -> Result<u32, u32> {
    // arg0 of `sysno` is the raw syscall number (`id: usize`), read directly
    // from the probed first-argument register — no dereference and no per-arch
    // `TrapFrame` layout assumption.
    let sysno = ctx.arg::<usize>(0).ok_or(0u32)? as u32;

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
