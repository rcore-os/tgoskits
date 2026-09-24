#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
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

/// One `iommu-map` entry from a PCI host bridge device tree node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIommuMapEntry {
    pub rid_base: u32,
    pub iommu_phandle: u32,
    pub stream_base: u32,
    pub length: u32,
}

/// Firmware routing for a PCI requester ID. The phandle is resolved by the
/// platform device registry before a driver is allowed to enable bus mastering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIommuTarget {
    pub iommu_phandle: u32,
    pub stream_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciIommuMapError {
    EmptyRange,
    RangeOverflow,
    InvalidRequesterIdRange,
    MaskedRequesterIdBase,
    AmbiguousRoute,
}

/// PCI requester-to-IOMMU routing supplied by firmware.
#[derive(Clone, Debug)]
pub struct PciIommuMap {
    mask: u32,
    entries: Vec<PciIommuMapEntry>,
}

impl PciIommuMap {
    pub fn new(mask: u32, entries: Vec<PciIommuMapEntry>) -> Result<Self, PciIommuMapError> {
        for entry in &entries {
            if entry.length == 0 {
                return Err(PciIommuMapError::EmptyRange);
            }
            if entry.rid_base.checked_add(entry.length - 1).is_none()
                || entry.stream_base.checked_add(entry.length - 1).is_none()
            {
                return Err(PciIommuMapError::RangeOverflow);
            }
            if entry.rid_base + entry.length - 1 > u16::MAX.into() {
                return Err(PciIommuMapError::InvalidRequesterIdRange);
            }
            if entry.rid_base & !mask != 0 {
                return Err(PciIommuMapError::MaskedRequesterIdBase);
            }
        }
        Ok(Self { mask, entries })
    }

    pub fn route(&self, requester_id: u16) -> Result<Option<PciIommuTarget>, PciIommuMapError> {
        let masked_id = u32::from(requester_id) & self.mask;
        let mut found = None;
        for entry in &self.entries {
            if masked_id >= entry.rid_base && masked_id - entry.rid_base < entry.length {
                if found.is_some() {
                    return Err(PciIommuMapError::AmbiguousRoute);
                }
                found = Some(PciIommuTarget {
                    iommu_phandle: entry.iommu_phandle,
                    stream_id: entry.stream_base + (masked_id - entry.rid_base),
                });
            }
        }
        Ok(found)
    }
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
    dma_coherent: bool,
    iommu_map: Option<PciIommuMap>,
}

impl PcieController {
    pub fn new(chip: impl Interface) -> Self {
        Self {
            chip: Arc::new(ChipRaw::new(chip)),
            bar_allocator: None,
            dma_coherent: false,
            iommu_map: None,
        }
    }

    pub fn set_dma_coherent(&mut self, dma_coherent: bool) {
        self.dma_coherent = dma_coherent;
    }

    pub fn dma_coherent(&self) -> bool {
        self.dma_coherent
    }

    pub fn set_iommu_map(&mut self, map: PciIommuMap) {
        self.iommu_map = Some(map);
    }

    pub fn iommu_map(&self) -> Option<&PciIommuMap> {
        self.iommu_map.as_ref()
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

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::{PciIommuMap, PciIommuMapEntry, PciIommuMapError};

    #[test]
    fn iommu_map_rejects_requester_base_ignored_by_mask() {
        let map = PciIommuMap::new(
            0xff,
            vec![PciIommuMapEntry {
                rid_base: 0x100,
                iommu_phandle: 1,
                stream_base: 0,
                length: 1,
            }],
        );
        assert!(matches!(map, Err(PciIommuMapError::MaskedRequesterIdBase)));
    }
}
