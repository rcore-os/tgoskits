mod config;

pub use config::*;
pub use pci_types::{
    Bar, CommandRegister, PciAddress, StatusRegister, capability::PciCapability,
    device_type::DeviceType,
};

#[derive(Debug, Clone, Copy)]
pub struct BusNumber {
    pub primary: u8,
    pub secondary: u8,
    pub subordinate: u8,
}
