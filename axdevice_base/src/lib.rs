#![no_std]

extern crate alloc;

#[macro_use]
extern crate log;

use axerrno::AxResult;
use axaddrspace::GuestPhysAddr;
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

pub trait BaseDeviceOps {
    fn emu_type(&self) -> EmuDeviceType;
    fn address_range(&self) -> AddrRange<GuestPhysAddr>;
    fn handle_read(&self, addr: GuestPhysAddr, width: usize) -> AxResult<usize>;
    fn handle_write(&self, addr: GuestPhysAddr, width: usize, val: usize);
}
