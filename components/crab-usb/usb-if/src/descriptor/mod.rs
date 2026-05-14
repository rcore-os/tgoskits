use core::num::NonZero;

use alloc::{string::String, vec::Vec};

use crate::transfer::Direction;

mod class_code;
mod lang_id;
mod parser;

pub use class_code::*;
pub use lang_id::*;
pub use parser::decode_string_descriptor;

#[repr(C)]
#[derive(Debug, Clone)]
pub struct DescriptorType(pub u8);

impl DescriptorType {
    pub const DEVICE: Self = Self(0x01);
    pub const CONFIGURATION: Self = Self(0x02);
    pub const STRING: Self = Self(0x03);
    pub const INTERFACE: Self = Self(0x04);
    pub const ENDPOINT: Self = Self(0x05);
    // Reserved 6
    // Reserved 7
    pub const INTERFACE_POWER: Self = Self(0x08);
    pub const OTG: Self = Self(0x09);
    pub const DEBUG: Self = Self(0x0A);
    pub const INTERFACE_ASSOCIATION: Self = Self(0x0B);
    pub const BOS: Self = Self(0x0F);
    pub const DEVICE_CAPABILITY: Self = Self(0x10);
    pub const SUPERSPEED_USB_ENDPOINT_COMPANION: Self = Self(0x30);
    pub const SUPERSPEEDPLUS_ISOCHRONOUS_ENDPOINT_COMPANION: Self = Self(0x31);
    pub const HUB: Self = Self(0x29);
}

impl From<u8> for DescriptorType {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<DescriptorType> for u8 {
    fn from(desc_type: DescriptorType) -> Self {
        desc_type.0
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DeviceDescriptorBase {
    pub length: u8,
    pub descriptor_type: u8,
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size_0: u8,
}

impl DeviceDescriptorBase {
    pub fn class(&self) -> Class {
        Class::from_class_and_subclass(self.class, self.subclass, self.protocol)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
    pub manufacturer_string_index: Option<NonZero<u8>>,
    pub product_string_index: Option<NonZero<u8>>,
    pub serial_number_string_index: Option<NonZero<u8>>,
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(data: &[u8]) -> Option<Self> {
        parser::DeviceDescriptor::new(data).map(Into::into)
    }

    pub const LEN: usize = 18;

    pub fn class(&self) -> Class {
        Class::from_class_and_subclass(self.class, self.subclass, self.protocol)
    }
}

#[derive(Debug, Clone)]
pub struct InterfaceDescriptor {
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub string_index: Option<NonZero<u8>>,
    pub string: Option<String>,
    pub num_endpoints: u8,
    pub endpoints: Vec<EndpointDescriptor>,
}

impl InterfaceDescriptor {
    pub fn class(&self) -> Class {
        Class::from_class_and_subclass(self.class, self.subclass, self.protocol)
    }
}

/// Endpoint type.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum EndpointType {
    /// Control endpoint.
    Control = 0,

    /// Isochronous endpoint.
    Isochronous = 1,

    /// Bulk endpoint.
    Bulk = 2,

    /// Interrupt endpoint.
    Interrupt = 3,
}

#[derive(Debug, Clone)]
pub struct EndpointDescriptor {
    pub address: u8,
    pub max_packet_size: u16,
    pub transfer_type: EndpointType,
    pub direction: Direction,
    pub packets_per_microframe: usize,
    pub interval: u8,
}

impl EndpointDescriptor {
    pub fn dci(&self) -> u8 {
        // DCI = (endpoint_number * 2) + direction
        // Control endpoint always has DCI 1
        let endpoint_number = self.address & 0x0F; // Extract endpoint number (low 4 bits)
        (endpoint_number * 2)
            + match self.transfer_type {
                EndpointType::Control => 1, // Control endpoint always has DCI 1
                _ => {
                    if self.direction == Direction::In {
                        1
                    } else {
                        0
                    }
                }
            }
    }
}

#[derive(Debug, Clone)]
pub struct InterfaceDescriptors {
    pub interface_number: u8,
    pub alt_settings: Vec<InterfaceDescriptor>,
}

impl InterfaceDescriptors {
    pub fn first_alt_setting(&self) -> InterfaceDescriptor {
        self.alt_settings.first().cloned().unwrap()
    }
}

#[derive(Debug, Clone)]
pub struct ConfigurationDescriptor {
    pub num_interfaces: u8,
    pub configuration_value: u8,
    pub attributes: u8,
    pub max_power: u8,
    pub string_index: Option<NonZero<u8>>,
    pub string: Option<String>,
    pub interfaces: Vec<InterfaceDescriptors>,
    pub raw: Vec<u8>,
}

impl ConfigurationDescriptor {
    pub fn parse(data: &[u8]) -> Option<Self> {
        parser::ConfigurationDescriptor::new(data).map(Into::into)
    }

    pub const LEN: usize = 9;
}

impl From<parser::DeviceDescriptor> for DeviceDescriptor {
    fn from(desc: parser::DeviceDescriptor) -> Self {
        DeviceDescriptor {
            usb_version: desc.usb_version(),
            class: desc.class(),
            subclass: desc.subclass(),
            protocol: desc.protocol(),
            max_packet_size_0: desc.max_packet_size_0(),
            vendor_id: desc.vendor_id(),
            product_id: desc.product_id(),
            device_version: desc.device_version(),
            manufacturer_string_index: desc.manufacturer_string_index(),
            product_string_index: desc.product_string_index(),
            serial_number_string_index: desc.serial_number_string_index(),
            num_configurations: desc.num_configurations(),
        }
    }
}

impl From<parser::EndpointDescriptor<'_>> for EndpointDescriptor {
    fn from(desc: parser::EndpointDescriptor) -> Self {
        EndpointDescriptor {
            address: desc.address(),
            max_packet_size: desc.max_packet_size() as _,
            direction: desc.direction(),
            transfer_type: desc.transfer_type(),
            packets_per_microframe: desc.packets_per_microframe() as usize,
            interval: desc.interval(),
        }
    }
}

impl From<parser::ConfigurationDescriptor<'_>> for ConfigurationDescriptor {
    fn from(desc: parser::ConfigurationDescriptor) -> Self {
        ConfigurationDescriptor {
            num_interfaces: desc.num_interfaces(),
            configuration_value: desc.configuration_value(),
            attributes: desc.attributes(),
            max_power: desc.max_power(),
            string_index: desc.string_index(),
            interfaces: desc.interfaces().map(InterfaceDescriptors::from).collect(),
            string: None,
            raw: desc.as_bytes().to_vec(),
        }
    }
}

impl From<parser::InterfaceDescriptor<'_>> for InterfaceDescriptor {
    fn from(desc: parser::InterfaceDescriptor) -> Self {
        InterfaceDescriptor {
            interface_number: desc.interface_number(),
            alternate_setting: desc.alternate_setting(),
            class: desc.class(),
            subclass: desc.subclass(),
            protocol: desc.protocol(),
            string_index: desc.string_index(),
            num_endpoints: desc.num_endpoints(),
            endpoints: desc.endpoints().map(EndpointDescriptor::from).collect(),
            string: None,
        }
    }
}

impl From<parser::InterfaceDescriptors<'_>> for InterfaceDescriptors {
    fn from(desc: parser::InterfaceDescriptors) -> Self {
        InterfaceDescriptors {
            interface_number: desc.interface_number(),
            alt_settings: desc.alt_settings().map(InterfaceDescriptor::from).collect(),
        }
    }
}
