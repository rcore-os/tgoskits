#![no_std]
#![no_main]

use aya_ebpf::{helpers::bpf_probe_read_user_str_bytes, macros::uprobe, programs::ProbeContext};
use aya_log_ebpf::info;

#[uprobe]
pub fn upb(ctx: ProbeContext) -> u32 {
    match try_upb(ctx) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

//  mkdir
fn try_upb(ctx: ProbeContext) -> Result<u32, u32> {
    let arg = ctx.arg::<*const u8>(0).unwrap();
    let arg2 = ctx.arg::<usize>(1).unwrap();
    let mut buf = [0u8; 256];
    let path = unsafe { bpf_probe_read_user_str_bytes(arg, &mut buf) }.map_err(|_| 1u32)?;
    let path = unsafe { core::str::from_utf8_unchecked(path) };
    info!(&ctx, "function called with args: {}, {:x}", path, arg2);
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
