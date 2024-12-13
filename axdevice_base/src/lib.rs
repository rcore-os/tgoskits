#![no_std]

//! This crate provides basic traits and structures for emulated devices of ArceOS hypervisor.
//!
//! This crate contains:
//! [`BaseDeviceOps`] trait: The trait that all emulated devices must implement.
//! [`EmulatedDeviceConfig`] struct: Represents the configuration of an emulated device for a virtual machine.
//! [`EmuDeviceType`] enum: Enumeration representing the type of emulator devices.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use memory_addr::AddrRange;

use axaddrspace::GuestPhysAddr;
use axerrno::AxResult;

mod emu_type;
// pub use emu_config_notuse::EmulatedDeviceConfig;
pub use emu_type::EmuDeviceType;

/// [`BaseDeviceOps`] is the trait that all emulated devices must implement.
pub trait BaseDeviceOps {
    /// Returns the type of the emulated device.
    fn emu_type(&self) -> EmuDeviceType;
    /// Returns the address range of the emulated device.
    fn address_range(&self) -> AddrRange<GuestPhysAddr>;
    /// Handles a read operation on the emulated device.
    fn handle_read(&self, addr: GuestPhysAddr, width: usize) -> AxResult<usize>;
    /// Handles a write operation on the emulated device.
    fn handle_write(&self, addr: GuestPhysAddr, width: usize, val: usize);
}
