mod cp210x;

use ax_errno::AxResult;

use self::cp210x::CP210X_BACKEND;
use crate::pseudofs::usbfs::{self, UsbDeviceHandle, UsbDeviceSnapshotInfo};

#[derive(Clone, Copy)]
pub(super) struct UsbSerialPortInfo {
    pub(super) bus_num: u8,
    pub(super) device_num: u8,
    pub(super) interface: u8,
    pub(super) bulk_in: u8,
    pub(super) bulk_out: u8,
    pub(super) backend: &'static dyn UsbSerialBackend,
}

impl UsbSerialPortInfo {
    pub(super) fn new(
        snapshot: &UsbDeviceSnapshotInfo,
        interface: u8,
        bulk_in: u8,
        bulk_out: u8,
        backend: &'static dyn UsbSerialBackend,
    ) -> Self {
        Self {
            bus_num: snapshot.bus_num,
            device_num: snapshot.device_num,
            interface,
            bulk_in,
            bulk_out,
            backend,
        }
    }
}

pub(super) trait UsbSerialBackend: Sync {
    fn name(&self) -> &'static str;
    fn probe(&'static self, snapshot: &UsbDeviceSnapshotInfo) -> Option<UsbSerialPortInfo>;
    fn init(&self, handle: &UsbDeviceHandle, port: &UsbSerialPortInfo, baud: u32) -> AxResult<()>;
    fn set_baud(
        &self,
        handle: &UsbDeviceHandle,
        port: &UsbSerialPortInfo,
        baud: u32,
    ) -> AxResult<()>;
}

// Keep chip-specific probing and setup behind a small backend table. Adding
// FTDI/CH34x/etc. should not require touching the tty state machine.
static USB_SERIAL_BACKENDS: [&'static dyn UsbSerialBackend; 1] = [&CP210X_BACKEND];

pub(super) fn find_usb_serial_port(index: usize) -> Option<UsbSerialPortInfo> {
    usbfs::usb_device_snapshots()
        .into_iter()
        .filter_map(|snapshot| {
            USB_SERIAL_BACKENDS
                .iter()
                .find_map(|backend| backend.probe(&snapshot))
        })
        .nth(index)
}
