#![no_std]
#![no_main]

use aya_ebpf::{EbpfContext, macros::raw_tracepoint, programs::RawTracePointContext};
use aya_log_ebpf::info;

#[raw_tracepoint(tracepoint = "sys_clone")]
pub fn rawtp(ctx: RawTracePointContext) -> i32 {
    match try_rawtp(ctx) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

// sys_clone(flags:u32, stack:usize, parent_tid:usize)
fn try_rawtp(ctx: RawTracePointContext) -> Result<i32, i32> {
    let args = ctx.as_ptr();
    let args = unsafe { &*(args as *const [u64; 3]) };
    let flags = args[0] as u32;
    let stack = args[1];
    let parent_tid = args[2];
    info!(
        &ctx,
        "sys_clone called with flags: {:x}, stack: 0x{:x}, parent_tid: {}",
        flags,
        stack,
        parent_tid
    );
    Ok(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";
