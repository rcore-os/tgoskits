#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use usb_if::{
    descriptor::{
        ConfigurationDescriptor, DeviceDescriptor, EndpointType, InterfaceDescriptor,
        parse_concatenated_config_descriptors,
    },
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
    pub interface_number: u8,
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

/// Probe the built-in USB serial chip families for one concrete interface.
pub fn probe_supported_port_for_interface(
    descriptor_blob: &[u8],
    interface_number: u8,
) -> Option<UsbSerialPortMatch> {
    cp210x::probe_interface(descriptor_blob, interface_number).map(|port| UsbSerialPortMatch {
        chip: UsbSerialChip::Cp210x,
        port,
    })
}

pub fn probe_supported_port_from_descriptors(
    descriptor: &DeviceDescriptor,
    configurations: &[ConfigurationDescriptor],
    interface_number: u8,
) -> Option<UsbSerialPortMatch> {
    cp210x::probe_interface_from_descriptors(descriptor, configurations, interface_number).map(
        |port| UsbSerialPortMatch {
            chip: UsbSerialChip::Cp210x,
            port,
        },
    )
}

pub fn device_id_from_descriptor_blob(blob: &[u8]) -> Option<UsbDeviceId> {
    let desc = DeviceDescriptor::parse(blob)?;
    Some(UsbDeviceId {
        vendor_id: desc.vendor_id,
        product_id: desc.product_id,
    })
}

pub(crate) fn bulk_pair_for_configurations(
    configurations: &[ConfigurationDescriptor],
    mut accept_interface: impl FnMut(&InterfaceDescriptor) -> bool,
) -> Option<UsbSerialPort> {
    for config in configurations {
        for interfaces in &config.interfaces {
            for interface in &interfaces.alt_settings {
                if accept_interface(interface)
                    && let Some(port) = bulk_pair_from_interface(interface)
                {
                    return Some(port);
                }
            }
        }
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
        interface_number: interface.interface_number,
        bulk_in: bulk_in?,
        bulk_out: bulk_out?,
    })
}

pub(crate) fn configurations_from_descriptor_blob(
    blob: &[u8],
) -> Option<Vec<ConfigurationDescriptor>> {
    let configurations = blob.get(DeviceDescriptor::LEN..)?;
    Some(parse_concatenated_config_descriptors(configurations).collect())
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

    fn configurations(blob: &[u8]) -> Vec<ConfigurationDescriptor> {
        parse_concatenated_config_descriptors(&blob[DeviceDescriptor::LEN..]).collect()
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
        let configurations = configurations(&blob);

        assert_eq!(
            bulk_pair_for_configurations(&configurations, |interface| interface.class == 0xff),
            Some(UsbSerialPort {
                interface_number: 3,
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
        let configurations = configurations(&blob);

        assert_eq!(
            bulk_pair_for_configurations(&configurations, |interface| interface.class == 0xff),
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
                    interface_number: 3,
                    bulk_in: 0x82,
                    bulk_out: 0x01,
                }
            })
        );
    }

    #[test]
    fn probe_supported_port_for_interface_selects_one_interface() {
        let first = interface(2, 0, 0xff, 0, 0);
        let second = interface(4, 0, 0xff, 0, 0);
        let config = config(&[
            &first,
            &endpoint(0x82, 0x02),
            &endpoint(0x01, 0x02),
            &second,
            &endpoint(0x84, 0x02),
            &endpoint(0x03, 0x02),
        ]);
        let blob = descriptor_blob(&[config]);

        assert_eq!(
            probe_supported_port_for_interface(&blob, 4),
            Some(UsbSerialPortMatch {
                chip: UsbSerialChip::Cp210x,
                port: UsbSerialPort {
                    interface_number: 4,
                    bulk_in: 0x84,
                    bulk_out: 0x03,
                }
            })
        );
        assert_eq!(probe_supported_port_for_interface(&blob, 5), None);
    }

    #[test]
    fn probe_supported_port_from_descriptors_selects_one_interface() {
        let first = interface(2, 0, 0xff, 0, 0);
        let second = interface(4, 0, 0xff, 0, 0);
        let config = config(&[
            &first,
            &endpoint(0x82, 0x02),
            &endpoint(0x01, 0x02),
            &second,
            &endpoint(0x84, 0x02),
            &endpoint(0x03, 0x02),
        ]);
        let blob = descriptor_blob(&[config]);
        let descriptor = DeviceDescriptor::parse(&blob).unwrap();
        let configurations = configurations(&blob);

        assert_eq!(
            probe_supported_port_from_descriptors(&descriptor, &configurations, 4),
            Some(UsbSerialPortMatch {
                chip: UsbSerialChip::Cp210x,
                port: UsbSerialPort {
                    interface_number: 4,
                    bulk_in: 0x84,
                    bulk_out: 0x03,
                }
            })
        );
        assert_eq!(
            probe_supported_port_from_descriptors(&descriptor, &configurations, 5),
            None
        );
    }
}
