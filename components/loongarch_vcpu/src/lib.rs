#![no_std]
#![cfg(target_arch = "loongarch64")]
#![allow(unsafe_op_in_unsafe_fn)]

#[macro_use]
extern crate log;

mod context_frame;
mod exception;
mod pcpu;
mod registers;
mod vcpu;

pub use self::{
    pcpu::LoongArchPerCpu,
    registers::*,
    vcpu::{LoongArchVCpu, LoongArchVCpuCreateConfig},
};

pub fn has_hardware_support() -> bool {
    let cpucfg2: u64;
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) cpucfg2, in(reg) 2);
    }
    (cpucfg2 & (1 << 10)) != 0
}
