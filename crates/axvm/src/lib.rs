#![no_std]
#![feature(asm_const)]
#![feature(concat_idents)]
#![feature(naked_functions)]
#![feature(const_trait_impl)]
#![cfg_attr(target_arch = "riscv64", feature(riscv_ext_intrinsics))]

//! This crate provides a minimal VM monitor (VMM) for running guest VMs.
//!
//! This crate contains:
//! - [`AxVM`]: The main structure representing a VM.
//!   - It currently handles only virtual CPUs
//!   - Memory mapping and device emulation are not implemented yet
//! - Implementations for [`axvcpu::AxArchVCpu`]: The architecture-dependent part of a virtual CPU.

extern crate alloc;
#[macro_use]
extern crate log;

mod hal;
mod mm;
mod percpu;
mod vm;

pub mod arch;
pub mod config;

pub use hal::AxVMHal;
pub use mm::NestedPageFaultInfo;
// pub use mm::{GuestPhysAddr, GuestVirtAddr, HostPhysAddr, HostVirtAddr};
pub use vm::AxVM;

pub type AxVMPerCpu<H> = percpu::AxVMPerCpu<arch::AxVMArchPerCpuImpl<H>>;

/// Whether the hardware has virtualization support.
pub fn has_hardware_support() -> bool {
    arch::has_hardware_support()
}
