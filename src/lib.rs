//! [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) guest VM address space management module.

#![no_std]
#![feature(const_trait_impl)]

#[macro_use]
extern crate log;
extern crate alloc;

mod address_space;
mod backend;
mod npt;

pub use address_space::*;

use axerrno::AxError;
use memory_addr::{PhysAddr, VirtAddr};
use memory_set::MappingError;

/// Guest virtual address.
pub type GuestVirtAddr = VirtAddr;
/// Guest physical address.
pub type GuestPhysAddr = VirtAddr;
/// Host virtual address.
pub type HostVirtAddr = VirtAddr;
/// Host physical address.
pub type HostPhysAddr = PhysAddr;

/// Information about nested page faults.
#[derive(Debug)]
pub struct NestedPageFaultInfo {
    /// Access type that caused the nested page fault.
    pub access_flags: MappingFlags,
    /// Guest physical address that caused the nested page fault.
    pub fault_guest_paddr: GuestPhysAddr,
}

fn mapping_err_to_ax_err(err: MappingError) -> AxError {
    warn!("Mapping error: {:?}", err);
    match err {
        MappingError::InvalidParam => AxError::InvalidInput,
        MappingError::AlreadyExists => AxError::AlreadyExists,
        MappingError::BadState => AxError::BadState,
    }
}
