use ax_errno::{AxError, AxResult};
use rdrive::{
    DriverGeneric, PlatformDevice,
    probe::{
        OnProbeError,
        usb::{ProbeUsb, UsbDeviceId, UsbInfo, UsbRemove},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
use spin::Once;
use usb_serial::{ControlTransfer, UsbSerialChip, UsbSerialPort, cp210x, probe_supported_port};

use crate::pseudofs::usbfs::UsbDeviceHandle;

const CP210X_IDS: &[UsbDeviceId] = &[UsbDeviceId {
    vendor_id: cp210x::VENDOR_ID,
    product_id: cp210x::PRODUCT_ID_EA60,
}];

static USB_SERIAL_REGISTER_ONCE: Once<()> = Once::new();

static USB_SERIAL_REGISTER: DriverRegister = DriverRegister {
    name: "USB serial tty",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: CP210X_IDS,
        classes: &[],
        on_probe: probe_usb_serial,
        on_remove: Some(remove_usb_serial),
    }],
};

#[derive(Clone, Copy)]
pub(super) struct UsbSerialPortInfo {
    pub(super) bus_num: u8,
    pub(super) device_num: u8,
    chip: UsbSerialChip,
    port: UsbSerialPort,
}

impl UsbSerialPortInfo {
    fn new(info: UsbInfo<'_>, chip: UsbSerialChip, port: UsbSerialPort) -> Self {
        Self {
            bus_num: info.bus_num(),
            device_num: info.device_num(),
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

impl DriverGeneric for UsbSerialPortInfo {
    fn name(&self) -> &str {
        self.name()
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

pub(super) fn register_usb_serial_probe() {
    USB_SERIAL_REGISTER_ONCE.call_once(|| {
        rdrive::register_add(USB_SERIAL_REGISTER.clone());
    });
}

fn probe_usb_serial(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let matched = probe_supported_port(info.descriptor_blob()).ok_or(OnProbeError::NotMatch)?;
    let platform: PlatformDevice = probe.into_platform_device();
    platform.register(UsbSerialPortInfo::new(info, matched.chip, matched.port));
    Ok(())
}

fn remove_usb_serial(remove: UsbRemove) {
    super::usb_serial_device_removed(remove.bus_num(), remove.device_num());
}

pub(super) fn find_usb_serial_port(index: usize) -> Option<UsbSerialPortInfo> {
    rdrive::get_list::<UsbSerialPortInfo>()
        .into_iter()
        .filter_map(|device| device.lock().ok().map(|guard| *guard))
        .nth(index)
}
