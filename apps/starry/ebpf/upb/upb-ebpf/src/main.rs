#![no_std]
#![no_main]

use aya_ebpf::{macros::uprobe, programs::ProbeContext};
use aya_log_ebpf::info;

#[uprobe]
pub fn upb(ctx: ProbeContext) -> u32 {
    match try_upb(ctx) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

fn try_upb(ctx: ProbeContext) -> Result<u32, u32> {
    let arg = ctx.arg::<u32>(0).unwrap();
    info!(&ctx, "function called with arg: {}", arg);
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
