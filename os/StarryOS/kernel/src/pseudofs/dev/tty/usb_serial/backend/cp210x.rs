use ax_errno::AxResult;

use super::{
    super::descriptors::{InterfaceDescriptor, bulk_pair_for_interface},
    UsbSerialBackend, UsbSerialPortInfo,
};
use crate::pseudofs::usbfs::{UsbDeviceHandle, UsbDeviceSnapshotInfo};

const USB_TYPE_VENDOR: u8 = 0x40;
const USB_RECIP_INTERFACE: u8 = 0x01;
const USB_DIR_OUT: u8 = 0x00;
const VENDOR_INTERFACE_OUT: u8 = USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE;

const CP210X_VENDOR_ID: u16 = 0x10c4;
const CP210X_PRODUCT_ID_EA60: u16 = 0xea60;
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

pub(super) static CP210X_BACKEND: Cp210xBackend = Cp210xBackend;

pub(super) struct Cp210xBackend;

impl UsbSerialBackend for Cp210xBackend {
    fn name(&self) -> &'static str {
        "cp210x"
    }

    fn probe(&'static self, snapshot: &UsbDeviceSnapshotInfo) -> Option<UsbSerialPortInfo> {
        if !matches!(
            (snapshot.vendor_id, snapshot.product_id),
            (CP210X_VENDOR_ID, CP210X_PRODUCT_ID_EA60)
        ) {
            return None;
        }

        let (interface, bulk_in, bulk_out) =
            bulk_pair_for_interface(&snapshot.descriptor_blob, cp210x_data_interface)?;
        Some(UsbSerialPortInfo::new(
            snapshot, interface, bulk_in, bulk_out, self,
        ))
    }

    fn init(&self, handle: &UsbDeviceHandle, port: &UsbSerialPortInfo, baud: u32) -> AxResult<()> {
        // Minimal Linux-compatible CP210x bring-up for a raw 8N1 UART: enable
        // the UART, set baud, set line control, assert DTR/RTS, then disable
        // hardware flow control.
        cp210x_request(
            handle,
            CP210X_IFC_ENABLE,
            CP210X_UART_ENABLE,
            port.interface,
            &mut [],
        )?;
        self.set_baud(handle, port, baud)?;
        cp210x_request(
            handle,
            CP210X_SET_LINE_CTL,
            CP210X_BITS_DATA_8,
            port.interface,
            &mut [],
        )?;
        cp210x_request(
            handle,
            CP210X_SET_MHS,
            CP210X_CONTROL_DTR
                | CP210X_CONTROL_RTS
                | CP210X_CONTROL_WRITE_DTR
                | CP210X_CONTROL_WRITE_RTS,
            port.interface,
            &mut [],
        )?;
        let mut flow = [0u8; 16];
        cp210x_request(handle, CP210X_SET_FLOW, 0, port.interface, &mut flow)?;
        Ok(())
    }

    fn set_baud(
        &self,
        handle: &UsbDeviceHandle,
        port: &UsbSerialPortInfo,
        baud: u32,
    ) -> AxResult<()> {
        let mut data = baud.to_le_bytes();
        cp210x_request(handle, CP210X_SET_BAUDRATE, 0, port.interface, &mut data)?;
        Ok(())
    }
}

fn cp210x_request(
    handle: &UsbDeviceHandle,
    request: u8,
    value: u16,
    interface: u8,
    data: &mut [u8],
) -> AxResult<usize> {
    handle.control_transfer(VENDOR_INTERFACE_OUT, request, value, interface as u16, data)
}

fn cp210x_data_interface(interface: InterfaceDescriptor) -> bool {
    interface.alternate_setting == 0
        && interface.class == USB_CLASS_VENDOR_SPECIFIC
        && interface.subclass == 0
        && interface.protocol == 0
}
