use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq as Mutex;
use rdrive::{
    DeviceId as RDriveDeviceId, DriverGeneric, PlatformDevice,
    probe::{
        OnProbeError,
        usb::{ProbeUsb, UsbClassId, UsbClassMatch, UsbDeviceId, UsbInfo, UsbRemove},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
use spin::{LazyLock, Once};
use usb_serial::{
    ControlTransfer, UsbSerialChip, UsbSerialPort, cp210x, probe_supported_port_for_interface,
};

use crate::pseudofs::usbfs::UsbDeviceHandle;

const CP210X_IDS: &[UsbDeviceId] = &[UsbDeviceId {
    vendor_id: cp210x::VENDOR_ID,
    product_id: cp210x::PRODUCT_ID_EA60,
}];

const CP210X_CLASSES: &[UsbClassMatch] = &[UsbClassMatch::Interface(UsbClassId::new(
    0xff,
    Some(0),
    Some(0),
))];

static USB_SERIAL_REGISTER_ONCE: Once<()> = Once::new();
static USB_SERIAL_SLOTS: LazyLock<Mutex<UsbSerialSlotTable>> =
    LazyLock::new(|| Mutex::new(UsbSerialSlotTable::default()));

static USB_SERIAL_REGISTER: DriverRegister = DriverRegister {
    name: "USB serial tty",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: CP210X_IDS,
        classes: CP210X_CLASSES,
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

#[derive(Clone, Copy)]
struct UsbSerialPortDevice {
    device_id: RDriveDeviceId,
    info: UsbSerialPortInfo,
}

#[derive(Default)]
struct UsbSerialSlotTable {
    by_device: BTreeMap<RDriveDeviceId, usize>,
    by_minor: BTreeMap<usize, RDriveDeviceId>,
}

impl UsbSerialSlotTable {
    fn reconcile(&mut self, devices: &[UsbSerialPortDevice]) {
        let present = devices
            .iter()
            .map(|device| device.device_id)
            .collect::<BTreeSet<_>>();
        let stale = self
            .by_device
            .keys()
            .copied()
            .filter(|device_id| !present.contains(device_id))
            .collect::<Vec<_>>();
        for device_id in stale {
            if let Some(minor) = self.by_device.remove(&device_id) {
                self.by_minor.remove(&minor);
            }
        }

        for device in devices {
            if self.by_device.contains_key(&device.device_id) {
                continue;
            }
            let minor = self.first_free_minor();
            self.by_device.insert(device.device_id, minor);
            self.by_minor.insert(minor, device.device_id);
        }
    }

    fn first_free_minor(&self) -> usize {
        let mut minor = 0;
        while self.by_minor.contains_key(&minor) {
            minor += 1;
        }
        minor
    }

    fn minors(&self) -> Vec<usize> {
        self.by_minor.keys().copied().collect()
    }

    fn device_for_minor(&self, minor: usize) -> Option<RDriveDeviceId> {
        self.by_minor.get(&minor).copied()
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
    let interface = info.interface().ok_or(OnProbeError::NotMatch)?;
    let matched =
        probe_supported_port_for_interface(info.descriptor_blob(), interface.interface_number)
            .ok_or(OnProbeError::NotMatch)?;
    let platform: PlatformDevice = probe.into_platform_device();
    platform.register(UsbSerialPortInfo::new(info, matched.chip, matched.port));
    Ok(())
}

fn remove_usb_serial(remove: UsbRemove) {
    super::usb_serial_device_removed(
        remove.bus_num(),
        remove.device_num(),
        remove
            .interface()
            .map(|interface| interface.interface_number),
    );
}

fn current_usb_serial_ports() -> Vec<UsbSerialPortDevice> {
    rdrive::get_list::<UsbSerialPortInfo>()
        .into_iter()
        .filter_map(|device| {
            let device_id = device.descriptor().device_id();
            device.lock().ok().map(|guard| UsbSerialPortDevice {
                device_id,
                info: *guard,
            })
        })
        .collect()
}

pub(super) fn find_usb_serial_port(minor: usize) -> Option<UsbSerialPortInfo> {
    let devices = current_usb_serial_ports();
    let device_id = {
        let mut slots = USB_SERIAL_SLOTS.lock();
        slots.reconcile(&devices);
        slots.device_for_minor(minor)?
    };
    devices
        .into_iter()
        .find_map(|device| (device.device_id == device_id).then_some(device.info))
}

pub(super) fn usb_serial_minors() -> Vec<usize> {
    let devices = current_usb_serial_ports();
    let mut slots = USB_SERIAL_SLOTS.lock();
    slots.reconcile(&devices);
    slots.minors()
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn test_device(device_id: RDriveDeviceId) -> UsbSerialPortDevice {
        UsbSerialPortDevice {
            device_id,
            info: UsbSerialPortInfo {
                bus_num: 1,
                device_num: 2,
                chip: UsbSerialChip::Cp210x,
                port: UsbSerialPort {
                    interface: 0,
                    bulk_in: 0x81,
                    bulk_out: 0x02,
                },
            },
        }
    }

    #[test]
    fn slot_table_keeps_existing_minor_when_lower_minor_is_removed() {
        let first = test_device(RDriveDeviceId::new());
        let second = test_device(RDriveDeviceId::new());
        let third = test_device(RDriveDeviceId::new());
        let mut slots = UsbSerialSlotTable::default();

        slots.reconcile(&[first, second]);
        assert_eq!(slots.device_for_minor(0), Some(first.device_id));
        assert_eq!(slots.device_for_minor(1), Some(second.device_id));

        slots.reconcile(&[second]);
        assert_eq!(slots.minors(), vec![1]);
        assert_eq!(slots.device_for_minor(1), Some(second.device_id));

        slots.reconcile(&[second, third]);
        assert_eq!(slots.device_for_minor(0), Some(third.device_id));
        assert_eq!(slots.device_for_minor(1), Some(second.device_id));
    }
}
