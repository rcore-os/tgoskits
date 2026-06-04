#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{kprobe, map},
    maps::HashMap,
    programs::ProbeContext,
};

// D1: aya_log uses perf-ringbuf which StarryOS's BpfPerfEvent does not yet
// implement (PERF-P0-1 in docs/ebpf-followup/perf-ringbuf-audit.md). Aya's
// logger initialization creates a ringbuf map fd, fails to mmap it, then
// drops the underlying Bpf view — closing the fd before the kernel-side
// preprocessor can resolve it. Removing info! lets the BPF program load
// successfully and feed counts via HashMap iter on the userspace side.

#[kprobe]
pub fn syscall_count(ctx: ProbeContext) -> u32 {
    try_syscall_count(ctx).unwrap_or_else(|ret| ret)
}

fn try_syscall_count(ctx: ProbeContext) -> Result<u32, u32> {
    let syscall_num = ctx.arg::<usize>(0).unwrap();
    if syscall_num != 1 {
        unsafe {
            if let Some(v) = SYSCALL_LIST.get(&(syscall_num as u32)) {
                let new_v = *v + 1;
                SYSCALL_LIST
                    .insert(&(syscall_num as u32), &new_v, 0)
                    .unwrap();
            } else {
                SYSCALL_LIST.insert(&(syscall_num as u32), &1, 0).unwrap();
            }
        }
    }
    Ok(0)
}

#[map]
static SYSCALL_LIST: HashMap<u32, u32> = HashMap::<u32, u32>::with_max_entries(1024, 0);

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // we need use this because the verifier will forbid loop
    unsafe { core::hint::unreachable_unchecked() }
    // loop{}
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
