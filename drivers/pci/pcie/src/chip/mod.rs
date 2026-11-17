use core::mem::{align_of, size_of};

use mmio_api::{MapError, Mmio, MmioAddr, MmioOp};
pub use rdif_pcie::PcieController;
use rdif_pcie::{DriverGeneric, Interface};

use crate::PciAddress;

mod layout;

pub use layout::ConfigSpaceLayout;

pub struct PcieGeneric {
    mmio: Mmio,
    layout: ConfigSpaceLayout,
}

impl PcieGeneric {
    pub fn new(
        mmio_base: impl Into<MmioAddr>,
        mmio_size: usize,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self, MapError> {
        Self::new_with_layout(mmio_base, mmio_size, ConfigSpaceLayout::Ecam, mmio_op)
    }

    /// Maps a configuration aperture using its firmware-selected layout.
    pub fn new_with_layout(
        mmio_base: impl Into<MmioAddr>,
        mmio_size: usize,
        layout: ConfigSpaceLayout,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self, MapError> {
        let mmio_base = mmio_base.into();
        if mmio_size < size_of::<u32>()
            || (layout == ConfigSpaceLayout::Loongson && mmio_size < layout.window_size(0))
            || !mmio_base.as_usize().is_multiple_of(align_of::<u32>())
            || mmio_base.as_usize().checked_add(mmio_size).is_none()
        {
            return Err(MapError::Invalid);
        }
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(mmio_base, mmio_size)?;
        Ok(Self { mmio, layout })
    }

    fn dword_offset(&self, address: PciAddress, register: u16) -> Option<usize> {
        if self.layout == ConfigSpaceLayout::Loongson
            && self.layout.window_size(address.bus()) > self.mmio.size()
        {
            return None;
        }
        let offset = self.layout.register_offset(address, register)?;
        (offset.is_multiple_of(align_of::<u32>())
            && offset.checked_add(size_of::<u32>())? <= self.mmio.size())
        .then_some(offset)
    }
}

impl DriverGeneric for PcieGeneric {
    fn name(&self) -> &str {
        "PciE Generic"
    }
}

impl Interface for PcieGeneric {
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        self.dword_offset(address, offset)
            .map_or(u32::MAX, |offset| self.mmio.read(offset))
    }

    fn write(&mut self, address: PciAddress, offset: u16, value: u32) {
        if let Some(offset) = self.dword_offset(address, offset) {
            self.mmio.write(offset, value);
        }
    }
}
