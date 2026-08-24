//! Narrow CDC ACM support for the audited SO-100 USB Control adapter.

use usb_if::descriptor::{ConfigurationDescriptor, DeviceDescriptor, InterfaceDescriptor};

use crate::{
    ControlTransfer, UsbDeviceId, UsbSerialPort, bulk_pair_for_interface,
    device_id_from_descriptor_blob,
};

pub const VENDOR_ID_QINHENG: u16 = 0x1a86;
pub const PRODUCT_ID_USB_SINGLE_SERIAL: u16 = 0x55d3;

const USB_CLASS_COMMUNICATIONS: u8 = 0x02;
const USB_CLASS_CDC_DATA: u8 = 0x0a;
const USB_CDC_SUBCLASS_ACM: u8 = 0x02;
const USB_CDC_PROTOCOL_AT: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CS_INTERFACE: u8 = 0x24;
const USB_CDC_UNION_TYPE: u8 = 0x06;

const USB_DIR_OUT: u8 = 0x00;
const USB_TYPE_CLASS: u8 = 0x20;
const USB_RECIP_INTERFACE: u8 = 0x01;
const CDC_INTERFACE_OUT: u8 = USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE;
const CDC_SET_LINE_CODING: u8 = 0x20;
const CDC_SET_CONTROL_LINE_STATE: u8 = 0x22;
const CDC_CONTROL_DTR: u16 = 1 << 0;
const CDC_CONTROL_RTS: u16 = 1 << 1;

/// Finds the two-interface CDC ACM function on the audited CH343 adapter.
pub fn probe(descriptor_blob: &[u8]) -> Option<UsbSerialPort> {
    let UsbDeviceId {
        vendor_id,
        product_id,
    } = device_id_from_descriptor_blob(descriptor_blob)?;
    if (vendor_id, product_id) != (VENDOR_ID_QINHENG, PRODUCT_ID_USB_SINGLE_SERIAL) {
        return None;
    }

    let mut rest = descriptor_blob.get(DeviceDescriptor::LEN..)?;
    while !rest.is_empty() {
        let config = ConfigurationDescriptor::parse(rest)?;
        for interfaces in &config.interfaces {
            for control in &interfaces.alt_settings {
                if !is_control_interface(control) {
                    continue;
                }
                let data_interface = union_data_interface(&config.raw, control.interface_number)?;
                let mut port = bulk_pair_for_interface(descriptor_blob, |interface| {
                    is_data_interface(interface) && interface.interface_number == data_interface
                })?;
                port.control_interface = control.interface_number;
                port.data_interface = data_interface;
                return Some(port);
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

/// Configures 8N1 line coding and asserts DTR/RTS on the ACM control interface.
pub fn init<T: ControlTransfer>(
    control: &T,
    port: &UsbSerialPort,
    baud: u32,
) -> Result<(), T::Error> {
    set_baud(control, port, baud)?;
    control.control_out(
        CDC_INTERFACE_OUT,
        CDC_SET_CONTROL_LINE_STATE,
        CDC_CONTROL_DTR | CDC_CONTROL_RTS,
        u16::from(port.control_interface),
        &mut [],
    )?;
    Ok(())
}

/// Updates the CDC ACM line coding while retaining fixed 8N1 framing.
pub fn set_baud<T: ControlTransfer>(
    control: &T,
    port: &UsbSerialPort,
    baud: u32,
) -> Result<(), T::Error> {
    let mut line_coding = [0u8; 7];
    line_coding[..4].copy_from_slice(&baud.to_le_bytes());
    line_coding[4] = 0; // one stop bit
    line_coding[5] = 0; // no parity
    line_coding[6] = 8;
    control.control_out(
        CDC_INTERFACE_OUT,
        CDC_SET_LINE_CODING,
        0,
        u16::from(port.control_interface),
        &mut line_coding,
    )?;
    Ok(())
}

fn is_control_interface(interface: &InterfaceDescriptor) -> bool {
    interface.alternate_setting == 0
        && interface.class == USB_CLASS_COMMUNICATIONS
        && interface.subclass == USB_CDC_SUBCLASS_ACM
        && interface.protocol == USB_CDC_PROTOCOL_AT
}

fn is_data_interface(interface: &InterfaceDescriptor) -> bool {
    interface.alternate_setting == 0
        && interface.class == USB_CLASS_CDC_DATA
        && interface.subclass == 0
        && interface.protocol == 0
}

fn union_data_interface(raw_config: &[u8], control_interface: u8) -> Option<u8> {
    let mut offset = 0usize;
    while offset < raw_config.len() {
        let length = usize::from(*raw_config.get(offset)?);
        if length < 2 || offset.checked_add(length)? > raw_config.len() {
            return None;
        }
        let descriptor = &raw_config[offset..offset + length];
        if descriptor[1] == USB_DESCRIPTOR_TYPE_CS_INTERFACE
            && length == 5
            && descriptor[2] == USB_CDC_UNION_TYPE
            && descriptor[3] == control_interface
        {
            return Some(descriptor[4]);
        }
        offset += length;
    }
    None
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use core::cell::RefCell;

    use super::*;

    type RecordedRequest = (u8, u8, u16, u16, Vec<u8>);

    #[derive(Default)]
    struct Recorder {
        requests: RefCell<Vec<RecordedRequest>>,
    }

    impl ControlTransfer for Recorder {
        type Error = ();

        fn control_out(
            &self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            data: &mut [u8],
        ) -> Result<usize, Self::Error> {
            self.requests
                .borrow_mut()
                .push((request_type, request, value, index, data.to_vec()));
            Ok(data.len())
        }
    }

    fn cdc_acm_blob(union: [u8; 5]) -> Vec<u8> {
        let descriptors: [&[u8]; 8] = [
            &[9, 0x04, 0, 0, 1, 0x02, 0x02, 0x01, 0],
            &[5, 0x24, 0x00, 0x10, 0x01],
            &[5, 0x24, 0x01, 0x00, 0x01],
            &[4, 0x24, 0x02, 0x02],
            &union,
            &[7, 0x05, 0x83, 0x03, 16, 0, 1],
            &[9, 0x04, 1, 0, 2, 0x0a, 0x00, 0x00, 0],
            &[7, 0x05, 0x02, 0x02, 32, 0, 0],
        ];
        let total_len = 9
            + descriptors
                .iter()
                .map(|descriptor| descriptor.len())
                .sum::<usize>()
            + 7;
        let mut config = vec![9, 0x02];
        config.extend_from_slice(&(total_len as u16).to_le_bytes());
        config.extend_from_slice(&[2, 1, 0, 0x80, 50]);
        for descriptor in descriptors {
            config.extend_from_slice(descriptor);
        }
        config.extend_from_slice(&[7, 0x05, 0x82, 0x02, 32, 0, 0]);

        let mut blob = vec![
            18, 0x01, 0x00, 0x02, 0x02, 0x00, 0x00, 64, 0x86, 0x1a, 0xd3, 0x55, 0x00, 0x04, 1, 2,
            3, 1,
        ];
        blob.extend_from_slice(&config);
        blob
    }

    #[test]
    fn probe_matches_audited_control_union_and_bulk_data_interface() {
        let blob = cdc_acm_blob([5, 0x24, 0x06, 0, 1]);

        assert_eq!(
            probe(&blob),
            Some(UsbSerialPort {
                control_interface: 0,
                data_interface: 1,
                bulk_in: 0x82,
                bulk_out: 0x02,
            })
        );
    }

    #[test]
    fn probe_rejects_wrong_identity_or_union_pair() {
        let mut wrong_identity = cdc_acm_blob([5, 0x24, 0x06, 0, 1]);
        wrong_identity[10] = 0x34;
        wrong_identity[11] = 0x12;
        assert!(probe(&wrong_identity).is_none());

        let wrong_union = cdc_acm_blob([5, 0x24, 0x06, 0, 2]);
        assert!(probe(&wrong_union).is_none());
    }

    #[test]
    fn init_emits_line_coding_then_control_state_on_control_interface() {
        let recorder = Recorder::default();
        let port = UsbSerialPort {
            control_interface: 4,
            data_interface: 7,
            bulk_in: 0x82,
            bulk_out: 0x02,
        };

        init(&recorder, &port, 1_000_000).unwrap();

        assert_eq!(
            recorder.requests.into_inner(),
            vec![
                (
                    CDC_INTERFACE_OUT,
                    CDC_SET_LINE_CODING,
                    0,
                    4,
                    vec![0x40, 0x42, 0x0f, 0x00, 0, 0, 8],
                ),
                (
                    CDC_INTERFACE_OUT,
                    CDC_SET_CONTROL_LINE_STATE,
                    CDC_CONTROL_DTR | CDC_CONTROL_RTS,
                    4,
                    vec![],
                ),
            ]
        );
    }
}
