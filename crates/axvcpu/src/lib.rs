#![no_std]

mod arch_vcpu;
mod exit;
mod vcpu;

pub use arch_vcpu::AxArchVCpu;
pub use vcpu::*;

// TODO: consider, should [`AccessWidth`] be moved to a new crate?
pub use exit::{AccessWidth, AxArchVCpuExitReason};

// TODO: move these definitions to memory_addr crate, or a new crate.
#[allow(unused_imports)]
pub use memory_addr::{PhysAddr as HostPhysAddr, VirtAddr as HostVirtAddr};
/// Guest virtual address.
pub type GuestVirtAddr = usize;
/// Guest physical address.
pub type GuestPhysAddr = usize;
