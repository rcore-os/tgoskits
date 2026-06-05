#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{kprobe, map},
    maps::HashMap,
    programs::ProbeContext,
};

// syscall_count: kprobe on `starry_kernel::syscall::sysno(id: usize)`, the
// `#[inline(never)]` helper `handle_syscall` calls once per syscall with the
// raw syscall number as its first argument. Probing it — instead of
// `handle_syscall`, whose arg0 is `&UserContext` — lets us read the syscall
// number straight out of the probed first-argument register via `ctx.arg(0)`,
// with no per-arch `TrapFrame` field offset. The kernel kprobe layer maps
// `arg(0)` onto the correct register for each arch (rdi / x0 / a0), so the
// same program is correct on x86_64/aarch64/riscv64/loongarch64. Counts go
// into a plain BPF_MAP_TYPE_HASH (no perf ringbuf / mmap dependency — the
// loader iterates it via bpf() map lookups).
//
// aya_log uses a perf-event ringbuf that StarryOS's BpfPerfEvent path does not
// surface to userspace, so this program logs nothing and feeds counts purely
// through the HashMap.
#[map]
static SYSCALL_LIST: HashMap<u32, u32> = HashMap::<u32, u32>::with_max_entries(1024, 0);

#[kprobe]
pub fn syscall_count(ctx: ProbeContext) -> u32 {
    try_syscall_count(ctx).unwrap_or_else(|ret| ret)
}

fn try_syscall_count(ctx: ProbeContext) -> Result<u32, u32> {
    // arg0 of `sysno` is the raw syscall number (`id: usize`), read directly
    // from the probed first-argument register — no dereference and no per-arch
    // `TrapFrame` layout assumption.
    let syscall_num = ctx.arg::<usize>(0).ok_or(1u32)? as u32;

    unsafe {
        if let Some(v) = SYSCALL_LIST.get(syscall_num) {
            let new_v = *v + 1;
            SYSCALL_LIST
                .insert(syscall_num, new_v, 0)
                .map_err(|_| 1u32)?;
        } else {
            SYSCALL_LIST.insert(syscall_num, 1, 0).map_err(|_| 1u32)?;
        }
    }
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // we need use this because the verifier will forbid loop
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
