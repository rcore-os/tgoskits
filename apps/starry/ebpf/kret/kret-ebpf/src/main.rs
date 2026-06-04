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
fn arg1(ctx: &ProbeContext) -> u64 {
    ctx.arg(2).unwrap()
}

#[cfg(not(bpf_target_arch = "x86_64"))]
fn arg1(ctx: &ProbeContext) -> u64 {
    ctx.arg(1).unwrap()
}

// pub fn sys_getpid() -> AxResult<isize>;
fn try_kret(ctx: RetProbeContext) -> Result<u32, u32> {
    let probe_context = ProbeContext::new(ctx.as_ptr());
    let a0 = probe_context.arg::<u64>(0).unwrap();
    let a1 = arg1(&probe_context);
    info!(
        &ctx,
        "Function (sys_getpid) returned: a0={}, a1={}, ", a0, a1
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
