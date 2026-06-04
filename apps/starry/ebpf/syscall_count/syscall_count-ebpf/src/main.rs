#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::bpf_probe_read,
    macros::{kprobe, map},
    maps::HashMap,
    programs::ProbeContext,
};

// syscall_count: kprobe on the central dispatcher
// `starry_kernel::syscall::handle_syscall(uctx: &mut UserContext)`, keeping a
// per-syscall-number hit count in a plain BPF_MAP_TYPE_HASH (no perf ringbuf /
// mmap dependency — the loader iterates it via bpf() map lookups).
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
    // arg0 of `handle_syscall` is the `&UserContext`, NOT the syscall number.
    // The saved user `rax`/`a7` (the syscall number) is the first `TrapFrame`
    // field, i.e. the first u64 of `UserContext`; dereference it with one
    // bpf_probe_read. Reading arg0 directly (as an earlier version did) counts
    // the *pointer value*, which is why it produced huge bogus keys.
    let uctx = ctx.arg::<usize>(0).ok_or(1u32)? as *const u64;
    // SAFETY: `uctx` is the live kernel-stack `UserContext` for the in-flight
    // syscall; reading its first u64 stays within that frame.
    let syscall_num = (unsafe { bpf_probe_read(uctx) }.map_err(|_| 1u32)?) as u32;

    unsafe {
        if let Some(v) = SYSCALL_LIST.get(&syscall_num) {
            let new_v = *v + 1;
            SYSCALL_LIST
                .insert(&syscall_num, &new_v, 0)
                .map_err(|_| 1u32)?;
        } else {
            SYSCALL_LIST.insert(&syscall_num, &1, 0).map_err(|_| 1u32)?;
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
