#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::HashMap,
    programs::TracePointContext,
};
use syscall_count_common::SYSCALL_ID_OFFSET;

#[tracepoint]
pub fn syscall_ebpf(ctx: TracePointContext) -> u32 {
    try_syscall_ebpf(ctx).unwrap_or_else(|ret| ret)
}

fn try_syscall_ebpf(ctx: TracePointContext) -> Result<u32, u32> {
    let syscall_num = unsafe { ctx.read_at::<i64>(SYSCALL_ID_OFFSET) }.map_err(|_| 1u32)?;
    if syscall_num >= 0 && syscall_num != 1 {
        let syscall_num = syscall_num as u32;
        unsafe {
            if let Some(v) = SYSCALL_LIST.get(&syscall_num) {
                let new_v = *v + 1;
                SYSCALL_LIST.insert(&syscall_num, &new_v, 0).unwrap();
            } else {
                SYSCALL_LIST.insert(&syscall_num, &1, 0).unwrap();
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
