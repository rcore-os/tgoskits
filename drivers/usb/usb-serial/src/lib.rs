#![no_std]

extern crate alloc;

use usb_if::{
    descriptor::{ConfigurationDescriptor, DeviceDescriptor, EndpointType, InterfaceDescriptor},
    transfer::Direction,
};

pub mod cp210x;

/// OS-side capability for class/vendor control OUT transfers.
///
/// The reusable USB serial code owns descriptor matching and chip command
/// layout; the integrating kernel owns how transfers are submitted.
pub trait ControlTransfer {
    type Error;

    fn control_out(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbDeviceId {
    pub vendor_id: u16,
    pub product_id: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbSerialPort {
    pub interface: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbSerialChip {
    Cp210x,
}

impl UsbSerialChip {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cp210x => "cp210x",
        }
    }

    pub fn init<T: ControlTransfer>(
        self,
        control: &T,
        port: &UsbSerialPort,
        baud: u32,
    ) -> Result<(), T::Error> {
        match self {
            Self::Cp210x => cp210x::init(control, port, baud),
        }
    }

    pub fn set_baud<T: ControlTransfer>(
        self,
        control: &T,
        port: &UsbSerialPort,
        baud: u32,
    ) -> Result<(), T::Error> {
        match self {
            Self::Cp210x => cp210x::set_baud(control, port, baud),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbSerialPortMatch {
    pub chip: UsbSerialChip,
    pub port: UsbSerialPort,
}

/// Probe the built-in USB serial chip families against a raw descriptor blob.
pub fn probe_supported_port(descriptor_blob: &[u8]) -> Option<UsbSerialPortMatch> {
    cp210x::probe(descriptor_blob).map(|port| UsbSerialPortMatch {
        chip: UsbSerialChip::Cp210x,
        port,
    })
}

pub fn device_id_from_descriptor_blob(blob: &[u8]) -> Option<UsbDeviceId> {
    let desc = DeviceDescriptor::parse(blob)?;
    Some(UsbDeviceId {
        vendor_id: desc.vendor_id,
        product_id: desc.product_id,
    })
}

pub fn bulk_pair_for_interface(
    blob: &[u8],
    mut accept_interface: impl FnMut(&InterfaceDescriptor) -> bool,
) -> Option<UsbSerialPort> {
    let mut rest = blob.get(DeviceDescriptor::LEN..)?;
    while !rest.is_empty() {
        let config = ConfigurationDescriptor::parse(rest)?;
        for interfaces in &config.interfaces {
            for interface in &interfaces.alt_settings {
                if accept_interface(interface)
                    && let Some(port) = bulk_pair_from_interface(interface)
                {
                    return Some(port);
                }
            }
        }
        let consumed = config.raw.len();
        if consumed == 0 || consumed > rest.len() {
            return None;
        }
        rest = &rest[consumed..];
    }
    None
}

fn bulk_pair_from_interface(interface: &InterfaceDescriptor) -> Option<UsbSerialPort> {
    let mut bulk_in = None;
    let mut bulk_out = None;
    for endpoint in &interface.endpoints {
        if endpoint.transfer_type != EndpointType::Bulk {
            continue;
        }
        match endpoint.direction {
            Direction::In => bulk_in = Some(endpoint.address),
            Direction::Out => bulk_out = Some(endpoint.address),
        }
    }

    Some(UsbSerialPort {
        interface: interface.interface_number,
        bulk_in: bulk_in?,
        bulk_out: bulk_out?,
    })
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    fn descriptor_blob(configs: &[Vec<u8>]) -> Vec<u8> {
        let mut blob = vec![
            18, 0x01, 0x00, 0x02, 0xff, 0x00, 0x00, 64, 0xc4, 0x10, 0x60, 0xea, 0x00, 0x01, 1, 2,
            3, 1,
        ];
        for config in configs {
            blob.extend_from_slice(config);
        }
        blob
    }

    fn config(descriptors: &[&[u8]]) -> Vec<u8> {
        let total_len = 9 + descriptors.iter().map(|desc| desc.len()).sum::<usize>();
        let mut config = vec![9, 0x02];
        config.extend_from_slice(&(total_len as u16).to_le_bytes());
        config.extend_from_slice(&[1, 1, 0, 0x80, 50]);
        for descriptor in descriptors {
            config.extend_from_slice(descriptor);
        }
        config
    }

    fn interface(
        number: u8,
        alternate_setting: u8,
        class: u8,
        subclass: u8,
        protocol: u8,
    ) -> [u8; 9] {
        [
            9,
            0x04,
            number,
            alternate_setting,
            2,
            class,
            subclass,
            protocol,
            0,
        ]
    }

    fn endpoint(address: u8, attributes: u8) -> [u8; 7] {
        [7, 0x05, address, attributes, 64, 0, 0]
    }

    #[test]
    fn device_id_from_descriptor_blob_decodes_device_descriptor() {
        let blob = descriptor_blob(&[]);

        assert_eq!(
            device_id_from_descriptor_blob(&blob),
            Some(UsbDeviceId {
                vendor_id: 0x10c4,
                product_id: 0xea60,
            })
        );
    }

    #[test]
    fn bulk_pair_selects_endpoints_from_accepted_interface() {
        let rejected = interface(0, 0, 0x02, 0x02, 0x01);
        let accepted = interface(3, 0, 0xff, 0, 0);
        let config = config(&[
            &rejected,
            &endpoint(0x83, 0x02),
            &endpoint(0x04, 0x02),
            &accepted,
            &endpoint(0x85, 0x03),
            &endpoint(0x02, 0x02),
            &endpoint(0x81, 0x02),
        ]);
        let blob = descriptor_blob(&[config]);

        assert_eq!(
            bulk_pair_for_interface(&blob, |interface| interface.class == 0xff),
            Some(UsbSerialPort {
                interface: 3,
                bulk_in: 0x81,
                bulk_out: 0x02,
            })
        );
    }

    #[test]
    fn bulk_pair_requires_both_directions_on_same_interface() {
        let only_in = interface(1, 0, 0xff, 0, 0);
        let only_out = interface(2, 0, 0xff, 0, 0);
        let config = config(&[
            &only_in,
            &endpoint(0x81, 0x02),
            &only_out,
            &endpoint(0x02, 0x02),
        ]);
        let blob = descriptor_blob(&[config]);

        assert_eq!(
            bulk_pair_for_interface(&blob, |interface| interface.class == 0xff),
            None
        );
    }

    #[test]
    fn probe_supported_port_returns_chip_and_port() {
        let accepted = interface(3, 0, 0xff, 0, 0);
        let config = config(&[&accepted, &endpoint(0x82, 0x02), &endpoint(0x01, 0x02)]);
        let blob = descriptor_blob(&[config]);

        assert_eq!(
            probe_supported_port(&blob),
            Some(UsbSerialPortMatch {
                chip: UsbSerialChip::Cp210x,
                port: UsbSerialPort {
                    interface: 3,
                    bulk_in: 0x82,
                    bulk_out: 0x01,
                }
            })
        );
    }
}
