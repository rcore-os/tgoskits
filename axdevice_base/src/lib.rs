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
use axaddrspace::GuestPhysAddr;
use axerrno::AxResult;
use memory_addr::AddrRange;

// TODO: support vgicv2
// pub(crate) mod emu_vgicdv2;
mod emu_type;
// pub use emu_config_notuse::EmulatedDeviceConfig;
pub use emu_type::EmuDeviceType;

/// Represents the configuration of an emulated device for a virtual machine.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmulatedDeviceConfig {
    /// The name of the device
    pub name: String,
    /// The base IPA (Intermediate Physical Address) of the device.
    pub base_ipa: usize,
    /// The length of the device.
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    pub irq_id: usize,
    /// The type of emulated device.
    pub emu_type: usize,
    /// The config_list of the device
    pub cfg_list: Vec<usize>,
}

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
