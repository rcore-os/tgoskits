use mmio_api::{MapError, Mmio, MmioAddr, MmioOp};
pub use rdif_pcie::PcieController;
use rdif_pcie::{DriverGeneric, Interface};

use crate::PciAddress;

pub struct PcieGeneric {
    mmio: Mmio,
}

impl PcieGeneric {
    pub fn new(
        mmio_base: impl Into<MmioAddr>,
        mmio_size: usize,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self, MapError> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(mmio_base.into(), mmio_size)?;
        Ok(Self { mmio })
    }

    fn mmio_offset(address: PciAddress, offset: u16) -> usize {
        ((address.bus() as u32) << 20
            | (address.device() as u32) << 15
            | (address.function() as u32) << 12
            | offset as u32) as usize
    }
}

impl DriverGeneric for PcieGeneric {
    fn name(&self) -> &str {
        "PciE Generic"
    }
}

impl Interface for PcieGeneric {
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        self.mmio.read(Self::mmio_offset(address, offset))
    }

    fn write(&mut self, address: PciAddress, offset: u16, value: u32) {
        self.mmio.write(Self::mmio_offset(address, offset), value)
    }
}
