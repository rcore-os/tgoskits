//! Basic traits and structures for emulated devices in ArceOS hypervisor.
//!
//! This crate contains:
//! - [`BaseDeviceOps`] trait: The trait that all emulated devices must implement.
//! - [`EmuDeviceType`] enum: Enumeration representing the type of emulator devices.
//!   (Already moved to `axvmconfig` crate.)
//! - [`EmulatedDeviceConfig`]: Configuration structure for device initialization.

#![no_std]
#![feature(trait_alias)]
// trait_upcasting has been stabilized in Rust 1.86, but we still need a while to update the minimum
// Rust version of Axvisor.
#![allow(stable_features)]
#![feature(trait_upcasting)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;

use axaddrspace::{
    GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, PortRange, SysRegAddrRange},
};
use axerrno::AxResult;

pub use axvmconfig::EmulatedDeviceType as EmuDeviceType;

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
pub trait BaseDeviceOps<R: DeviceAddrRange>: Any {
    /// Returns the type of the emulated device.
    fn emu_type(&self) -> EmuDeviceType;
    /// Returns the address range of the emulated device.
    fn address_range(&self) -> R;
    /// Handles a read operation on the emulated device.
    fn handle_read(&self, addr: R::Addr, width: AccessWidth) -> AxResult<usize>;
    /// Handles a write operation on the emulated device.
    fn handle_write(&self, addr: R::Addr, width: AccessWidth, val: usize) -> AxResult;
}

/// Determines whether the given device is of type `T` and calls the provided function `f` with a
/// reference to the device if it is.
pub fn map_device_of_type<T: BaseDeviceOps<R>, R: DeviceAddrRange, U, F: FnOnce(&T) -> U>(
    device: &Arc<dyn BaseDeviceOps<R>>,
    f: F,
) -> Option<U> {
    let any_arc: Arc<dyn Any> = device.clone();

    any_arc.downcast_ref::<T>().map(f)
}

// trait aliases are limited yet: https://github.com/rust-lang/rfcs/pull/3437
/// [`BaseMmioDeviceOps`] is the trait that all emulated MMIO devices must implement.
/// It is a trait alias of [`BaseDeviceOps`] with [`GuestPhysAddrRange`] as the address range.
pub trait BaseMmioDeviceOps = BaseDeviceOps<GuestPhysAddrRange>;
/// [`BaseSysRegDeviceOps`] is the trait that all emulated system register devices must implement.
/// It is a trait alias of [`BaseDeviceOps`] with [`SysRegAddrRange`] as the address range.
pub trait BaseSysRegDeviceOps = BaseDeviceOps<SysRegAddrRange>;
/// [`BasePortDeviceOps`] is the trait that all emulated port devices must implement.
/// It is a trait alias of [`BaseDeviceOps`] with [`PortRange`] as the address range.
pub trait BasePortDeviceOps = BaseDeviceOps<PortRange>;

#[cfg(test)]
mod test;
