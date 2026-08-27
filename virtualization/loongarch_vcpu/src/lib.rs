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

#[cfg(test)]
mod world_switch_tests;

pub use self::{
    context_frame::LoongArchContextFrame,
    exception::handle_exception_irq,
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

/// Re-enables host interrupts immediately before returning to the host scheduler.
///
/// # Safety
///
/// The caller must have fully left guest mode and restored the host address
/// space and exception entry before allowing interrupts to be delivered.
pub unsafe fn prepare_host_scheduler_yield() {
    let current_crmd = registers::csr_read::<{ registers::CSR_CRMD }>();
    registers::csr_write::<{ registers::CSR_CRMD }>(current_crmd | registers::CSR_CRMD_IE);
}
