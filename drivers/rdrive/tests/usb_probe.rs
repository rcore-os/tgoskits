use core::sync::atomic::{AtomicUsize, Ordering};

use rdrive::{
    DriverGeneric, Platform, PlatformDevice, get_list,
    probe::{
        OnProbeError,
        usb::{ProbeUsb, UsbClassMatch, UsbClassPattern, UsbDevice, UsbDeviceId, UsbDeviceKey},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static NOT_MATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
static PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
static CLASS_PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
static DEVICE_CLASS_PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
static REMOVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SWITCH_A_PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SWITCH_A_REMOVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SWITCH_B_PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);

const CP210X_EA60: UsbDeviceId = UsbDeviceId {
    vendor_id: 0x10c4,
    product_id: 0xea60,
};

const FTDI_6001: UsbDeviceId = UsbDeviceId {
    vendor_id: 0x0403,
    product_id: 0x6001,
};

#[derive(Clone, Copy)]
struct UsbTestDevice {
    bus_num: u8,
    device_num: u8,
    interface_number: Option<u8>,
}

impl DriverGeneric for UsbTestDevice {
    fn name(&self) -> &str {
        "UsbTestDevice"
    }
}

struct SwitchDeviceA;
struct SwitchDeviceB;

impl DriverGeneric for SwitchDeviceA {
    fn name(&self) -> &str {
        "SwitchDeviceA"
    }
}

impl DriverGeneric for SwitchDeviceB {
    fn name(&self) -> &str {
        "SwitchDeviceB"
    }
}

fn probe_not_match(_probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    NOT_MATCH_COUNT.fetch_add(1, Ordering::SeqCst);
    Err(OnProbeError::NotMatch)
}

fn probe_usb(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let (info, platform): (_, PlatformDevice) = probe.into_parts();
    assert_eq!(info.device_id(), Some(CP210X_EA60));
    assert_eq!(info.descriptor_blob().len(), 18);
    platform.register(UsbTestDevice {
        bus_num: info.bus_num(),
        device_num: info.device_num(),
        interface_number: info.interface().map(|interface| interface.interface_number),
    });
    Ok(())
}

fn probe_usb_class(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    CLASS_PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let (info, platform): (_, PlatformDevice) = probe.into_parts();
    let device_id = info.device_id().ok_or(OnProbeError::NotMatch)?;
    assert_eq!(
        info.device_descriptor()
            .ok_or(OnProbeError::NotMatch)?
            .vendor_id,
        device_id.vendor_id
    );
    assert_eq!(
        info.configurations()
            .first()
            .ok_or(OnProbeError::NotMatch)?
            .configuration_value,
        1
    );
    let interface = info.interface().ok_or(OnProbeError::NotMatch)?;
    assert_eq!(interface.interface_number, 4);
    assert_eq!(interface.alternate_setting, 0);
    platform.register(UsbTestDevice {
        bus_num: info.bus_num(),
        device_num: info.device_num(),
        interface_number: Some(interface.interface_number),
    });
    Ok(())
}

fn probe_usb_device_class(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    DEVICE_CLASS_PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let (info, platform): (_, PlatformDevice) = probe.into_parts();
    let device_class = info.device_class().ok_or(OnProbeError::NotMatch)?;
    assert_eq!(device_class.class, 0x09);
    assert!(info.interface().is_none());
    platform.register(UsbTestDevice {
        bus_num: info.bus_num(),
        device_num: info.device_num(),
        interface_number: None,
    });
    Ok(())
}

fn probe_switch_a(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    SWITCH_A_PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let (_info, platform) = probe.into_parts();
    platform.register(SwitchDeviceA);
    Ok(())
}

fn remove_switch_a(_remove: rdrive::probe::usb::UsbRemove) {
    SWITCH_A_REMOVE_COUNT.fetch_add(1, Ordering::SeqCst);
}

fn probe_switch_b(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    SWITCH_B_PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    let (_info, platform) = probe.into_parts();
    platform.register(SwitchDeviceB);
    Ok(())
}

fn remove_usb(_remove: rdrive::probe::usb::UsbRemove) {
    REMOVE_COUNT.fetch_add(1, Ordering::SeqCst);
}

static USB_NOT_MATCH_REGISTER: DriverRegister = DriverRegister {
    name: "usb negative test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[CP210X_EA60],
        classes: &[],
        on_probe: probe_not_match,
        on_remove: None,
    }],
};

static USB_REGISTER: DriverRegister = DriverRegister {
    name: "usb test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[CP210X_EA60],
        classes: &[],
        on_probe: probe_usb,
        on_remove: Some(remove_usb),
    }],
};

static USB_OTHER_REGISTER: DriverRegister = DriverRegister {
    name: "usb other test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[FTDI_6001],
        classes: &[],
        on_probe: probe_usb,
        on_remove: None,
    }],
};

static USB_HID_CLASS_REGISTER: DriverRegister = DriverRegister {
    name: "usb hid class test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[],
        classes: &[UsbClassMatch::Interface(UsbClassPattern::new(
            0x03, None, None,
        ))],
        on_probe: probe_usb_class,
        on_remove: None,
    }],
};

static USB_HUB_DEVICE_CLASS_REGISTER: DriverRegister = DriverRegister {
    name: "usb hub device class test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[],
        classes: &[UsbClassMatch::Device(UsbClassPattern::new(
            0x09,
            Some(0),
            Some(0),
        ))],
        on_probe: probe_usb_device_class,
        on_remove: None,
    }],
};

static USB_SWITCH_A_REGISTER: DriverRegister = DriverRegister {
    name: "usb switch test driver A",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[],
        classes: &[UsbClassMatch::Interface(UsbClassPattern::new(
            0x05, None, None,
        ))],
        on_probe: probe_switch_a,
        on_remove: Some(remove_switch_a),
    }],
};

static USB_SWITCH_B_REGISTER: DriverRegister = DriverRegister {
    name: "usb switch test driver B",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[],
        classes: &[UsbClassMatch::Interface(UsbClassPattern::new(
            0x06, None, None,
        ))],
        on_probe: probe_switch_b,
        on_remove: None,
    }],
};

fn device_descriptor(vendor_id: u16, product_id: u16) -> Vec<u8> {
    let mut blob = vec![18, 0x01, 0x00, 0x02, 0xff, 0, 0, 64];
    blob.extend_from_slice(&vendor_id.to_le_bytes());
    blob.extend_from_slice(&product_id.to_le_bytes());
    blob.extend_from_slice(&[0x00, 0x01, 1, 2, 3, 1]);
    blob
}

fn device_descriptor_with_class(class: u8, subclass: u8, protocol: u8) -> Vec<u8> {
    let mut blob = device_descriptor(0x1234, 0x5678);
    blob[4] = class;
    blob[5] = subclass;
    blob[6] = protocol;
    blob
}

fn device_with_interface_class(class: u8, subclass: u8, protocol: u8) -> Vec<u8> {
    let mut blob = device_descriptor(0x1234, 0x5678);
    blob.extend_from_slice(&[9, 0x02, 18, 0, 1, 1, 0, 0x80, 50]);
    blob.extend_from_slice(&[9, 0x04, 4, 0, 0, class, subclass, protocol, 0]);
    blob
}

#[test]
fn usb_probe_matches_ids_and_populates_each_device_once() {
    rdrive::init(Platform::Static).expect("static platform should init");
    rdrive::register_add(USB_NOT_MATCH_REGISTER.clone());
    rdrive::register_add(USB_REGISTER.clone());
    rdrive::register_add(USB_OTHER_REGISTER.clone());
    rdrive::register_add(USB_HID_CLASS_REGISTER.clone());
    rdrive::register_add(USB_HUB_DEVICE_CLASS_REGISTER.clone());
    rdrive::register_add(USB_SWITCH_A_REGISTER.clone());
    rdrive::register_add(USB_SWITCH_B_REGISTER.clone());

    let host = rdrive::DeviceId::new();
    let cp210x = UsbDevice::new(
        UsbDeviceKey::new(host, 7),
        1,
        2,
        device_descriptor(0x10c4, 0xea60),
    );
    let unmatched = UsbDevice::new(
        UsbDeviceKey::new(host, 8),
        1,
        3,
        device_descriptor(0x1234, 0x5678),
    );
    let hid = UsbDevice::new(
        UsbDeviceKey::new(host, 9),
        1,
        4,
        device_with_interface_class(0x03, 0x01, 0x01),
    );
    let hub = UsbDevice::new(
        UsbDeviceKey::new(host, 10),
        1,
        5,
        device_descriptor_with_class(0x09, 0, 0),
    );
    let switch_a = UsbDevice::new(
        UsbDeviceKey::new(host, 11),
        1,
        6,
        device_with_interface_class(0x05, 0, 0),
    );

    rdrive::sync_usb_devices_for_host(
        host,
        &[
            cp210x.clone(),
            unmatched,
            hid.clone(),
            hub.clone(),
            switch_a,
        ],
        true,
    )
    .expect("USB probe should succeed");
    assert_eq!(SWITCH_A_PROBE_COUNT.load(Ordering::SeqCst), 1);

    rdrive::probe_usb_devices_for_host(host, &[], true)
        .expect("incremental USB probe should not remove existing devices");
    assert_eq!(SWITCH_A_REMOVE_COUNT.load(Ordering::SeqCst), 0);
    assert_eq!(get_list::<UsbTestDevice>().len(), 3);
    assert_eq!(get_list::<SwitchDeviceA>().len(), 1);

    let switch_b = UsbDevice::new(
        UsbDeviceKey::new(host, 11),
        1,
        6,
        device_with_interface_class(0x06, 0, 0),
    );
    rdrive::sync_usb_devices_for_host(
        host,
        &[cp210x.clone(), hid.clone(), hub.clone(), switch_b],
        true,
    )
    .expect("repeat USB probe should succeed");

    assert_eq!(NOT_MATCH_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(PROBE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(CLASS_PROBE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(DEVICE_CLASS_PROBE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(REMOVE_COUNT.load(Ordering::SeqCst), 0);
    assert_eq!(SWITCH_A_REMOVE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(SWITCH_B_PROBE_COUNT.load(Ordering::SeqCst), 1);

    let devices = get_list::<UsbTestDevice>();
    assert_eq!(devices.len(), 3);
    assert!(devices.iter().any(|device| {
        let device = device.lock().expect("registered USB device should lock");
        device.bus_num == 1 && device.device_num == 2 && device.interface_number.is_none()
    }));
    assert!(devices.iter().any(|device| {
        let device = device.lock().expect("registered USB device should lock");
        device.bus_num == 1 && device.device_num == 4 && device.interface_number == Some(4)
    }));
    assert!(devices.iter().any(|device| {
        let device = device.lock().expect("registered USB device should lock");
        device.bus_num == 1 && device.device_num == 5 && device.interface_number.is_none()
    }));
    assert!(get_list::<SwitchDeviceA>().is_empty());
    assert_eq!(get_list::<SwitchDeviceB>().len(), 1);

    rdrive::sync_usb_devices_for_host(host, &[], true).expect("USB remove should succeed");

    assert_eq!(REMOVE_COUNT.load(Ordering::SeqCst), 1);
    assert!(get_list::<UsbTestDevice>().is_empty());
    assert!(get_list::<SwitchDeviceB>().is_empty());
}
