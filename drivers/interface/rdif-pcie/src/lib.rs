#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc};
use core::cell::UnsafeCell;

use pci_types::ConfigRegionAccess;
pub use pci_types::PciAddress;
pub use rdif_base::{DriverGeneric, KError};

pub mod addr_alloc;
mod bar_alloc;

pub use bar_alloc::SimpleBarAllocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciMem32 {
    pub address: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciMem64 {
    pub address: u64,
    pub size: u64,
}

impl rdif_base::DriverGeneric for PcieController {
    fn name(&self) -> &str {
        self.as_ref().name()
    }

    // fn raw_any(&self) -> Option<&dyn core::any::Any> {
    //     Some(self.chip.as_mut() as &dyn core::any::Any)
    // }
    // fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
    //     Some(self.chip.as_mut() as &mut dyn core::any::Any)
    // }
}

pub trait Interface: DriverGeneric {
    /// Performs a PCI read at `address` with `offset`.
    ///
    /// # Safety
    ///
    /// `address` and `offset` must be valid for PCI reads.
    fn read(&mut self, address: PciAddress, offset: u16) -> u32;

    /// Performs a PCI write at `address` with `offset`.
    ///
    /// # Safety
    ///
    /// `address` and `offset` must be valid for PCI writes.
    fn write(&mut self, address: PciAddress, offset: u16, value: u32);
}

pub struct PcieController {
    chip: Arc<ChipRaw>,
    pub bar_allocator: Option<SimpleBarAllocator>,
}

impl PcieController {
    pub fn new(chip: impl Interface) -> Self {
        Self {
            chip: Arc::new(ChipRaw::new(chip)),
            bar_allocator: None,
        }
    }
    pub fn typed_ref<T: Interface>(&self) -> Option<&T> {
        self.raw_any()?.downcast_ref()
    }
    pub fn typed_mut<T: Interface>(&mut self) -> Option<&mut T> {
        self.raw_any_mut()?.downcast_mut()
    }

    fn as_ref(&self) -> &dyn Interface {
        unsafe { &*self.chip.0.get() }.as_ref()
    }

    pub fn config_access(&mut self, address: PciAddress) -> ConfigAccess {
        ConfigAccess {
            address,
            chip: self.chip.clone(),
        }
    }

    pub fn set_mem32(&mut self, space: PciMem32, perfetchable: bool) {
        let al = self.bar_allocator.get_or_insert_default();
        al.set_mem32(space, perfetchable).unwrap();
    }

    pub fn set_mem64(&mut self, space: PciMem64, perfetchable: bool) {
        let al = self.bar_allocator.get_or_insert_default();
        al.set_mem64(space, perfetchable).unwrap();
    }
}

impl ConfigRegionAccess for PcieController {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        unsafe { (*self.chip.0.get()).read(address, offset) }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        unsafe { (*self.chip.0.get()).write(address, offset, value) }
    }
}

pub struct ConfigAccess {
    address: PciAddress,
    chip: Arc<ChipRaw>,
}

impl ConfigRegionAccess for ConfigAccess {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        assert!(address == self.address);
        unsafe { (*self.chip.0.get()).read(self.address, offset) }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        assert!(address == self.address);
        unsafe { (*self.chip.0.get()).write(self.address, offset, value) }
    }
}

struct ChipRaw(UnsafeCell<Box<dyn Interface>>);

unsafe impl Send for ChipRaw {}
unsafe impl Sync for ChipRaw {}

impl ChipRaw {
    fn new(chip: impl Interface) -> Self {
        Self(UnsafeCell::new(Box::new(chip)))
    }
}
