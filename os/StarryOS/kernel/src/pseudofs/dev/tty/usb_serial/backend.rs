use ax_errno::{AxError, AxResult};
use usb_serial::{ControlTransfer, UsbSerialChip, UsbSerialPort, probe_supported_port};

use crate::pseudofs::usbfs::{self, UsbDeviceHandle, UsbDeviceSnapshotInfo};

#[derive(Clone, Copy)]
pub(super) struct UsbSerialPortInfo {
    pub(super) bus_num: u8,
    pub(super) device_num: u8,
    chip: UsbSerialChip,
    port: UsbSerialPort,
}

impl UsbSerialPortInfo {
    fn new(snapshot: &UsbDeviceSnapshotInfo, chip: UsbSerialChip, port: UsbSerialPort) -> Self {
        Self {
            bus_num: snapshot.bus_num,
            device_num: snapshot.device_num,
            chip,
            port,
        }
    }

    pub(super) fn name(&self) -> &'static str {
        self.chip.name()
    }

    pub(super) fn interface(&self) -> u8 {
        self.port.interface
    }

    pub(super) fn bulk_in(&self) -> u8 {
        self.port.bulk_in
    }

    pub(super) fn bulk_out(&self) -> u8 {
        self.port.bulk_out
    }

    pub(super) fn init(&self, handle: &UsbDeviceHandle, baud: u32) -> AxResult<()> {
        self.chip.init(&StarryControl(handle), &self.port, baud)
    }

    pub(super) fn set_baud(&self, handle: &UsbDeviceHandle, baud: u32) -> AxResult<()> {
        self.chip.set_baud(&StarryControl(handle), &self.port, baud)
    }
}

struct StarryControl<'a>(&'a UsbDeviceHandle);

impl ControlTransfer for StarryControl<'_> {
    type Error = AxError;

    fn control_out(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, Self::Error> {
        self.0
            .control_transfer(request_type, request, value, index, data)
    }
}

pub(super) fn find_usb_serial_port(index: usize) -> Option<UsbSerialPortInfo> {
    usbfs::usb_device_snapshots()
        .into_iter()
        .filter_map(|snapshot| {
            let matched = probe_supported_port(&snapshot.descriptor_blob)?;
            Some(UsbSerialPortInfo::new(
                &snapshot,
                matched.chip,
                matched.port,
            ))
        })
        .nth(index)
}
