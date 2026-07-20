#![cfg(target_arch = "loongarch64")]
#![no_std]
#![allow(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod context_frame;
mod exception;
mod guest_addr;
mod guest_csr;
pub mod host;
mod host_cpu;
mod iocsr;
mod mmio;
mod pcpu;
pub mod registers;
mod trap;
mod types;
mod vcpu;

pub use self::{
    context_frame::LoongArchContextFrame,
    exception::{handle_exception_irq, handle_exception_sync},
    host::LoongArchHostOps,
    iocsr::{LoongArchIocsrState, LoongArchIocsrStateRef},
    pcpu::LoongArchPerCpu,
    trap::TrapKind,
    types::{
        LoongArchAccessFlags, LoongArchAccessWidth, LoongArchGuestPhysAddr, LoongArchGuestVirtAddr,
        LoongArchHostPhysAddr, LoongArchHostVirtAddr, LoongArchNestedPagingConfig,
        LoongArchVcpuError, LoongArchVcpuId, LoongArchVcpuResult, LoongArchVmExit, LoongArchVmId,
    },
    vcpu::{LoongArchVCpu, LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig, LoongArchVcpu},
};

pub fn has_hardware_support() -> bool {
    let cpucfg2: u64;
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) cpucfg2, in(reg) 2);
    }
    (cpucfg2 & (1 << 10)) != 0
}
