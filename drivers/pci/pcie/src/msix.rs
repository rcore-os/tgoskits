use core::{ops::Range, ptr::NonNull};

use pci_types::capability::MsixCapability;
pub use rdif_msi::MsiMessage;
use thiserror::Error;
use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

register_structs! {
    #[allow(non_snake_case)]
    MsixTableEntryRegs {
        (0x00 => MessageAddressLow: ReadWrite<u32>),
        (0x04 => MessageAddressHigh: ReadWrite<u32>),
        (0x08 => MessageData: ReadWrite<u32>),
        (0x0c => VectorControl: ReadWrite<u32, VectorControl::Register>),
        (0x10 => @END),
    }
}

register_bitfields! [
    u32,
    VectorControl [
        Mask OFFSET(0) NUMBITS(1) []
    ]
];

#[derive(Clone, Copy)]
pub struct MsixTableEntry {
    regs: NonNull<MsixTableEntryRegs>,
}

impl MsixTableEntry {
    /// Creates an MSI-X table entry view over a mapped 16-byte table entry.
    ///
    /// # Safety
    ///
    /// `raw` must point to a valid, writable MSI-X table entry and remain valid
    /// for the lifetime of the returned view. The caller must ensure exclusive
    /// device configuration access while programming the entry.
    pub unsafe fn from_raw(raw: *mut u32) -> Self {
        Self {
            regs: NonNull::new(raw.cast()).expect("MSI-X table entry pointer must not be null"),
        }
    }

    pub fn program_masked(&self, message: MsiMessage) {
        self.mask();
        self.regs().MessageAddressLow.set(message.address as u32);
        self.regs()
            .MessageAddressHigh
            .set((message.address >> 32) as u32);
        self.regs().MessageData.set(message.data);
    }

    pub fn mask(&self) {
        self.regs().VectorControl.modify(VectorControl::Mask::SET);
    }

    pub fn unmask(&self) {
        self.regs().VectorControl.modify(VectorControl::Mask::CLEAR);
    }

    pub fn is_masked(&self) -> bool {
        self.regs().VectorControl.is_set(VectorControl::Mask)
    }

    fn regs(&self) -> &MsixTableEntryRegs {
        unsafe { self.regs.as_ref() }
    }
}

unsafe impl Send for MsixTableEntry {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsixTableInfo {
    pub bar: u8,
    pub offset: usize,
    pub entries: u16,
    pub pba_bar: u8,
    pub pba_offset: usize,
}

impl MsixTableInfo {
    pub fn from_capability(capability: &MsixCapability) -> Self {
        Self {
            bar: capability.table_bar(),
            offset: capability.table_offset() as usize,
            entries: capability.table_size(),
            pba_bar: capability.pba_bar(),
            pba_offset: capability.pba_offset() as usize,
        }
    }

    pub const fn byte_len(self) -> usize {
        self.entries as usize * core::mem::size_of::<MsixTableEntryRegs>()
    }

    pub fn table_range(self, bar_range: Range<usize>) -> Result<Range<usize>, MsixError> {
        let start = bar_range
            .start
            .checked_add(self.offset)
            .ok_or(MsixError::TableOutsideBar)?;
        let end = start
            .checked_add(self.byte_len())
            .ok_or(MsixError::TableOutsideBar)?;
        if end > bar_range.end {
            return Err(MsixError::TableOutsideBar);
        }
        Ok(start..end)
    }
}

pub struct MsixTableRegion {
    base: NonNull<u8>,
    entries: u16,
}

unsafe impl Send for MsixTableRegion {}

impl MsixTableRegion {
    /// Creates an MSI-X table view over a mapped MSI-X table range.
    ///
    /// # Safety
    ///
    /// `base` must point to the first MSI-X table entry of a writable mapped
    /// BAR range that contains `entries` complete table entries. The caller must
    /// keep the mapping alive and serialize configuration-time table writes.
    pub unsafe fn new(base: NonNull<u8>, entries: u16) -> Self {
        Self { base, entries }
    }

    pub const fn entries(&self) -> u16 {
        self.entries
    }

    pub fn entry(&self, index: u16) -> Result<MsixTableEntry, MsixError> {
        if index >= self.entries {
            return Err(MsixError::InvalidVector);
        }
        let offset = usize::from(index) * core::mem::size_of::<MsixTableEntryRegs>();
        let ptr = unsafe { self.base.as_ptr().add(offset) }.cast();
        Ok(unsafe { MsixTableEntry::from_raw(ptr) })
    }

    pub fn program_masked(&self, index: u16, message: MsiMessage) -> Result<(), MsixError> {
        self.entry(index)?.program_masked(message);
        Ok(())
    }

    pub fn mask(&self, index: u16) -> Result<(), MsixError> {
        self.entry(index)?.mask();
        Ok(())
    }

    pub fn unmask(&self, index: u16) -> Result<(), MsixError> {
        self.entry(index)?.unmask();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MsixError {
    #[error("PCI endpoint has no MSI-X capability")]
    MissingCapability,
    #[error("MSI-X vector index is outside the table")]
    InvalidVector,
    #[error("MSI-X table BAR is not a memory BAR")]
    InvalidTableBar,
    #[error("MSI-X table extends outside its BAR")]
    TableOutsideBar,
}
