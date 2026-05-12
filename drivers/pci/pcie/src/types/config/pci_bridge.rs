use core::{fmt::Debug, ops::Deref};

use bit_field::BitField;
use pci_types::{ConfigRegionAccess, PciPciBridgeHeader};
use rdif_pcie::ConfigAccess;

use super::PciHeaderBase;

pub struct PciPciBridge {
    base: Option<PciHeaderBase>,
    header: Option<PciPciBridgeHeader>,
    is_root: bool,
}

impl PciPciBridge {
    pub(crate) fn root() -> Self {
        Self {
            base: None,
            header: None,
            is_root: true,
        }
    }

    pub(crate) fn new(base: PciHeaderBase) -> Self {
        let header = PciPciBridgeHeader::from_header(base.header(), &base.root)
            .expect("PciPciBridgeHeader::from_header failed");

        Self {
            base: Some(base),
            header: Some(header),
            is_root: false,
        }
    }

    fn header(&self) -> &PciPciBridgeHeader {
        self.header.as_ref().expect("Not a root bridge")
    }

    fn access(&self) -> &ConfigAccess {
        &self.base.as_ref().expect("Not a root bridge").root
    }

    pub fn primary_bus_number(&self) -> u8 {
        if self.is_root {
            return 0;
        }
        self.header().primary_bus_number(self.access())
    }

    pub fn secondary_bus_number(&self) -> u8 {
        if self.is_root {
            return 0;
        }
        self.header().secondary_bus_number(self.access())
    }

    pub fn subordinate_bus_number(&self) -> u8 {
        if self.is_root {
            return 0;
        }
        self.header().subordinate_bus_number(self.access())
    }

    pub fn update_bus_number<F>(&mut self, f: F)
    where
        F: FnOnce(BusNumber) -> BusNumber,
    {
        if self.is_root {
            return;
        }
        let address = self.base.as_ref().unwrap().address();
        let mut data = unsafe { self.access().read(address, 0x18) };
        let new_bus = f(BusNumber {
            primary: data.get_bits(0..8) as u8,
            secondary: data.get_bits(8..16) as u8,
            subordinate: data.get_bits(16..24) as u8,
        });
        data.set_bits(16..24, new_bus.subordinate.into());
        data.set_bits(8..16, new_bus.secondary.into());
        data.set_bits(0..8, new_bus.primary.into());
        unsafe {
            self.access().write(address, 0x18, data);
        }
    }
}

pub struct BusNumber {
    pub primary: u8,
    pub secondary: u8,
    pub subordinate: u8,
}

impl Deref for PciPciBridge {
    type Target = PciHeaderBase;

    fn deref(&self) -> &Self::Target {
        self.base.as_ref().expect("Not a root bridge")
    }
}

impl Debug for PciPciBridge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PciPciBridge")
            .field("base", &self.base.as_ref().expect("Not a root bridge"))
            .field("primary_bus", &self.primary_bus_number())
            .field("secondary_bus", &self.secondary_bus_number())
            .field("subordinate_bus", &self.subordinate_bus_number())
            .finish()
    }
}
