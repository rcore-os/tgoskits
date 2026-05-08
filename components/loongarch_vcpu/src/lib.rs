#![no_std]
#![allow(unsafe_op_in_unsafe_fn)]

mod context_frame;
mod exception;
mod pcpu;
mod registers;
mod vcpu;

pub use self::{
    context_frame::{LoongArchContextFrame, LoongArchGuestSystemRegisters},
    exception::{TrapKind, handle_exception_irq, handle_exception_sync},
    pcpu::LoongArchPerCpu,
    registers::inject_interrupt,
    vcpu::{LoongArchVCpu, LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig},
};

#[cfg(target_arch = "loongarch64")]
pub fn has_hardware_support() -> bool {
    let cpucfg2: u64;
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) cpucfg2, in(reg) 2);
    }
    (cpucfg2 & (1 << 10)) != 0
}

#[cfg(not(target_arch = "loongarch64"))]
pub const fn has_hardware_support() -> bool {
    false
}
