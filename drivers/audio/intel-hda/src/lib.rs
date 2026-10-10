#![no_std]
//! Portable Intel High Definition Audio controller and codec transport core.
//!
//! PCI enumeration and BAR mapping remain the platform adapter's responsibility.
//! The adapter transfers an owned mmio_api::Mmio into MappedHdaIo, supplies
//! a delay/time capability, and passes the device-scoped DMA capability to
//! Controller. Construction is hardware-active and may reset the controller,
//! enumerate codecs, and configure a playback route; it must be called only
//! after the owner has admitted the device and acquired its resources.

extern crate alloc;

mod bringup;
mod codec;
mod desc;
mod regs;

pub use bringup::Controller;
pub use codec::{Route, Widget};
pub use dma_api::{DeviceDma, DmaError};
pub use mmio_api::{Mmio, MmioRaw};
use thiserror::Error;

/// The Intel HDA PCI function admitted by the core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HdaDevice {
    vendor_id: u16,
    device_id: u16,
}

impl HdaDevice {
    /// Intel PCI vendor identifier.
    pub const fn vendor_id(self) -> u16 {
        self.vendor_id
    }

    /// PCI device identifier.
    pub const fn device_id(self) -> u16 {
        self.device_id
    }
}

/// Admits an Intel High Definition Audio controller PCI function.
///
/// PCI class code must be multimedia/audio (04:03:00). A matching class from
/// another vendor is intentionally rejected because only Intel controller
/// behavior is implemented and reviewed in this crate.
pub const fn identify(
    vendor_id: u16,
    device_id: u16,
    class: u8,
    subclass: u8,
    programming_interface: u8,
) -> Option<HdaDevice> {
    if vendor_id != 0x8086 || class != 0x04 || subclass != 0x03 || programming_interface != 0 {
        return None;
    }
    Some(HdaDevice {
        vendor_id,
        device_id,
    })
}

/// Register access width supported by the HDA register block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterWidth {
    /// One-byte access.
    Byte,
    /// Two-byte access.
    Word,
    /// Four-byte access.
    Dword,
}

impl RegisterWidth {
    const fn bytes(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }
}

/// Controller I/O and time capability.
///
/// Implementations must use device/MMIO access semantics, honor the supplied
/// access width, and keep all registers within the reported window. Callers
/// must serialize access to a single controller; the core does not add locks.
pub trait HdaIo {
    /// Length of the currently mapped register window.
    fn mapped_len(&self) -> usize;

    /// Reads one HDA register with its specified width.
    fn read(&mut self, offset: usize, width: RegisterWidth) -> u32;

    /// Writes one HDA register with its specified width.
    fn write(&mut self, offset: usize, width: RegisterWidth, value: u32);

    /// Delays for at least the requested duration.
    fn delay_us(&mut self, micros: u32);

    /// Monotonic elapsed time in nanoseconds.
    fn now_ns(&self) -> u64;
}

/// Platform timing source required by HDA reset and playback lifecycle.
pub trait Clock {
    /// Delays for at least the requested duration.
    fn delay_us(&mut self, micros: u32);
    /// Returns monotonic elapsed time in nanoseconds.
    fn now_ns(&self) -> u64;
}

/// HDA MMIO capability backed by the TGOSKits MMIO mapping API.
pub struct MappedHdaIo<C> {
    mmio: Mmio,
    clock: C,
}

impl<C> MappedHdaIo<C> {
    /// Transfers ownership of the already-mapped BAR into the HDA core.
    ///
    /// The mapping is retained for this object's lifetime and unmapped by
    /// mmio-api when it is dropped.
    pub const fn new(mmio: Mmio, clock: C) -> Self {
        Self { mmio, clock }
    }
}

impl<C: Clock> HdaIo for MappedHdaIo<C> {
    fn mapped_len(&self) -> usize {
        self.mmio.size()
    }

    fn read(&mut self, offset: usize, width: RegisterWidth) -> u32 {
        assert!(checked_register_range(self.mmio.size(), offset, width).is_ok());
        match width {
            RegisterWidth::Byte => u32::from(self.mmio.read::<u8>(offset)),
            RegisterWidth::Word => u32::from(self.mmio.read::<u16>(offset)),
            RegisterWidth::Dword => self.mmio.read::<u32>(offset),
        }
    }

    fn write(&mut self, offset: usize, width: RegisterWidth, value: u32) {
        assert!(checked_register_range(self.mmio.size(), offset, width).is_ok());
        match width {
            RegisterWidth::Byte => self.mmio.write(offset, value as u8),
            RegisterWidth::Word => self.mmio.write(offset, value as u16),
            RegisterWidth::Dword => self.mmio.write(offset, value),
        }
    }

    fn delay_us(&mut self, micros: u32) {
        self.clock.delay_us(micros);
    }

    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }
}

fn checked_register_range(size: usize, offset: usize, width: RegisterWidth) -> Result {
    let Some(end) = offset.checked_add(width.bytes()) else {
        return Err(Error::RegisterRange);
    };
    if end > size || !offset.is_multiple_of(width.bytes()) {
        return Err(Error::RegisterRange);
    }
    Ok(())
}

/// Errors from HDA setup, codec discovery, stream operations, and capabilities.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum Error {
    /// The PCI function, controller capability, codec route, or PCM mode is unsupported.
    #[error("unsupported HDA device or capability")]
    Unsupported,
    /// Invalid arguments or malformed hardware-reported topology.
    #[error("invalid HDA input or register-reported topology")]
    InvalidParam,
    /// The MMIO window is too short or an access is out of range/misaligned.
    #[error("HDA register access is outside the mapped window")]
    RegisterRange,
    /// A register operation or hardware completion failed.
    #[error("HDA register or codec operation failed")]
    Io,
    /// The controller has entered a state where safe reuse is not proven.
    #[error("HDA controller state is no longer usable")]
    BadState,
    /// The requested operation conflicts with outstanding playback periods.
    #[error("HDA playback resources are busy")]
    ResourceBusy,
    /// No playback period is currently available.
    #[error("HDA playback queue has no available period")]
    Again,
    /// The DMA capability rejected an allocation or violated its constraints.
    #[error(transparent)]
    Dma(#[from] DmaError),
    /// A DMA address was not aligned or did not fit the HDA controller address width.
    #[error("HDA DMA allocation does not satisfy alignment or address-width constraints")]
    DmaAddress,
}

/// Result type for this controller core.
pub type Result<T = ()> = core::result::Result<T, Error>;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

#[cfg(feature = "rdif")]
mod rdif;
