use usb_if::descriptor::InterfaceDescriptor;

use crate::{
    ControlTransfer, UsbDeviceId, UsbSerialPort, bulk_pair_for_interface,
    device_id_from_descriptor_blob,
};

pub const VENDOR_ID: u16 = 0x10c4;
pub const PRODUCT_ID_EA60: u16 = 0xea60;

const USB_TYPE_VENDOR: u8 = 0x40;
const USB_RECIP_INTERFACE: u8 = 0x01;
const USB_DIR_OUT: u8 = 0x00;
pub const VENDOR_INTERFACE_OUT: u8 = USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE;

const CP210X_IFC_ENABLE: u8 = 0x00;
const CP210X_SET_LINE_CTL: u8 = 0x03;
const CP210X_SET_MHS: u8 = 0x07;
const CP210X_SET_FLOW: u8 = 0x13;
const CP210X_SET_BAUDRATE: u8 = 0x1e;
const CP210X_UART_ENABLE: u16 = 0x0001;
const CP210X_BITS_DATA_8: u16 = 0x0800;
const CP210X_CONTROL_DTR: u16 = 0x0001;
const CP210X_CONTROL_RTS: u16 = 0x0002;
const CP210X_CONTROL_WRITE_DTR: u16 = 0x0100;
const CP210X_CONTROL_WRITE_RTS: u16 = 0x0200;
const USB_CLASS_VENDOR_SPECIFIC: u8 = 0xff;

pub fn probe(descriptor_blob: &[u8]) -> Option<UsbSerialPort> {
    let UsbDeviceId {
        vendor_id,
        product_id,
    } = device_id_from_descriptor_blob(descriptor_blob)?;
    if !matches!((vendor_id, product_id), (VENDOR_ID, PRODUCT_ID_EA60)) {
        return None;
    }

    bulk_pair_for_interface(descriptor_blob, is_data_interface)
}

pub fn init<T: ControlTransfer>(
    control: &T,
    port: &UsbSerialPort,
    baud: u32,
) -> Result<(), T::Error> {
    // Minimal Linux-compatible bring-up for a raw 8N1 UART: enable the UART,
    // set baud, set line control, assert DTR/RTS, then disable hardware flow
    // control.
    cp210x_request(
        control,
        CP210X_IFC_ENABLE,
        CP210X_UART_ENABLE,
        port.interface,
        &mut [],
    )?;
    set_baud(control, port, baud)?;
    cp210x_request(
        control,
        CP210X_SET_LINE_CTL,
        CP210X_BITS_DATA_8,
        port.interface,
        &mut [],
    )?;
    cp210x_request(
        control,
        CP210X_SET_MHS,
        CP210X_CONTROL_DTR
            | CP210X_CONTROL_RTS
            | CP210X_CONTROL_WRITE_DTR
            | CP210X_CONTROL_WRITE_RTS,
        port.interface,
        &mut [],
    )?;
    let mut flow = [0u8; 16];
    cp210x_request(control, CP210X_SET_FLOW, 0, port.interface, &mut flow)?;
    Ok(())
}

pub fn set_baud<T: ControlTransfer>(
    control: &T,
    port: &UsbSerialPort,
    baud: u32,
) -> Result<(), T::Error> {
    let mut data = baud.to_le_bytes();
    cp210x_request(control, CP210X_SET_BAUDRATE, 0, port.interface, &mut data)?;
    Ok(())
}

fn cp210x_request<T: ControlTransfer>(
    control: &T,
    request: u8,
    value: u16,
    interface: u8,
    data: &mut [u8],
) -> Result<usize, T::Error> {
    control.control_out(VENDOR_INTERFACE_OUT, request, value, interface as u16, data)
}

fn is_data_interface(interface: &InterfaceDescriptor) -> bool {
    interface.alternate_setting == 0
        && interface.class == USB_CLASS_VENDOR_SPECIFIC
        && interface.subclass == 0
        && interface.protocol == 0
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

    fn cp210x_blob(interface: [u8; 9]) -> Vec<u8> {
        let mut config = vec![9, 0x02];
        config.extend_from_slice(&(9u16 + 9 + 7 + 7).to_le_bytes());
        config.extend_from_slice(&[1, 1, 0, 0x80, 50]);
        config.extend_from_slice(&interface);
        config.extend_from_slice(&[7, 0x05, 0x01, 0x02, 64, 0, 0]);
        config.extend_from_slice(&[7, 0x05, 0x82, 0x02, 64, 0, 0]);

        let mut blob = vec![
            18, 0x01, 0x00, 0x02, 0xff, 0x00, 0x00, 64, 0xc4, 0x10, 0x60, 0xea, 0x00, 0x01, 1, 2,
            3, 1,
        ];
        blob.extend_from_slice(&config);
        blob
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

    #[test]
    fn probe_accepts_cp210x_ea60_vendor_interface() {
        let blob = cp210x_blob(interface(1, 0, USB_CLASS_VENDOR_SPECIFIC, 0, 0));

        assert_eq!(
            probe(&blob),
            Some(UsbSerialPort {
                interface: 1,
                bulk_in: 0x82,
                bulk_out: 0x01,
            })
        );
    }

    #[test]
    fn probe_rejects_non_cp210x_device_id() {
        let mut blob = cp210x_blob(interface(1, 0, USB_CLASS_VENDOR_SPECIFIC, 0, 0));
        blob[10] = 0x01;
        blob[11] = 0x00;

        assert!(probe(&blob).is_none());
    }

    #[test]
    fn probe_rejects_non_vendor_or_nonzero_alt_interface() {
        let cdc_blob = cp210x_blob(interface(1, 0, 0x02, 0x02, 0x01));
        assert!(probe(&cdc_blob).is_none());

        let alternate_blob = cp210x_blob(interface(1, 1, USB_CLASS_VENDOR_SPECIFIC, 0, 0));
        assert!(probe(&alternate_blob).is_none());
    }

    #[test]
    fn init_emits_cp210x_control_sequence() {
        let recorder = Recorder::default();
        let port = UsbSerialPort {
            interface: 2,
            bulk_in: 0x82,
            bulk_out: 0x01,
        };

        init(&recorder, &port, 115_200).unwrap();

        assert_eq!(
            recorder.requests.into_inner(),
            vec![
                (
                    VENDOR_INTERFACE_OUT,
                    CP210X_IFC_ENABLE,
                    CP210X_UART_ENABLE,
                    2,
                    vec![]
                ),
                (
                    VENDOR_INTERFACE_OUT,
                    CP210X_SET_BAUDRATE,
                    0,
                    2,
                    115_200u32.to_le_bytes().to_vec()
                ),
                (
                    VENDOR_INTERFACE_OUT,
                    CP210X_SET_LINE_CTL,
                    CP210X_BITS_DATA_8,
                    2,
                    vec![]
                ),
                (
                    VENDOR_INTERFACE_OUT,
                    CP210X_SET_MHS,
                    CP210X_CONTROL_DTR
                        | CP210X_CONTROL_RTS
                        | CP210X_CONTROL_WRITE_DTR
                        | CP210X_CONTROL_WRITE_RTS,
                    2,
                    vec![]
                ),
                (VENDOR_INTERFACE_OUT, CP210X_SET_FLOW, 0, 2, vec![0; 16]),
            ]
        );
    }
}
