#![no_std]
#![feature(doc_cfg)]
#![feature(naked_functions)]
#![feature(riscv_ext_intrinsics)]
#![feature(asm_const)]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;

/// The Control and Status Registers (CSRs) for a RISC-V hypervisor.
pub mod csrs;
mod detect;
mod percpu;
mod regs;
mod vcpu;

pub use self::percpu::RISCVPerCpu;
pub use self::vcpu::RISCVVCpu;
pub use detect::detect_h_extension as has_hardware_support;

/// Low-level resource interfaces that must be implemented by the crate user.
#[crate_interface::def_interface]
pub trait HalIf {
    /// Returns the physical address of the given virtual address.
    fn virt_to_phys(vaddr: axaddrspace::HostVirtAddr) -> axaddrspace::HostPhysAddr;
}
