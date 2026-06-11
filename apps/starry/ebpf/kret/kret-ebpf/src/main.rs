#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]
use aya_ebpf::{
    EbpfContext,
    macros::kretprobe,
    programs::{ProbeContext, RetProbeContext},
};
use aya_log_ebpf::info;

#[kretprobe]
pub fn kret(ctx: RetProbeContext) -> u32 {
    match try_kret(ctx) {
        Ok(ret) => ret,
        Err(ret) => ret,
    }
}

#[cfg(bpf_target_arch = "x86_64")]
fn ret1_value(ctx: &ProbeContext) -> u64 {
    // rdx
    ctx.arg(2).unwrap()
}

#[cfg(not(bpf_target_arch = "x86_64"))]
fn ret1_value(ctx: &ProbeContext) -> u64 {
    ctx.arg(1).unwrap()
}

// pub fn sys_getpid() -> AxResult<isize>;
fn try_kret(ctx: RetProbeContext) -> Result<u32, u32> {
    let probe_context = ProbeContext::new(ctx.as_ptr());
    let ret0 = ctx.ret::<u64>();
    let ret1 = ret1_value(&probe_context);
    info!(
        &ctx,
        "Function (sys_getpid) returned: a0={}, a1={}", ret0, ret1
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
