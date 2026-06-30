use core::sync::atomic::{AtomicUsize, Ordering};
use std::{sync::mpsc, thread, time::Duration};

use rdrive::{
    DriverGeneric, Platform, PlatformDevice, get_list,
    probe::{
        OnProbeError,
        usb::{ProbeUsb, UsbDevice, UsbDeviceId, UsbDeviceKey},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static REENTER_COUNT: AtomicUsize = AtomicUsize::new(0);

const CP210X_EA60: UsbDeviceId = UsbDeviceId {
    vendor_id: 0x10c4,
    product_id: 0xea60,
};

struct ReentrantDevice;

impl DriverGeneric for ReentrantDevice {
    fn name(&self) -> &str {
        "ReentrantDevice"
    }
}

fn probe_reentrant(probe: ProbeUsb<'_>) -> Result<(), OnProbeError> {
    let count = REENTER_COUNT.fetch_add(1, Ordering::SeqCst);
    let (info, platform): (_, PlatformDevice) = probe.into_parts();
    if count == 0 {
        let device = UsbDevice::new(
            info.key(),
            info.bus_num(),
            info.device_num(),
            info.descriptor_blob().to_vec(),
        );
        rdrive::probe_usb_devices_for_host(info.key().host, &[device], true)
            .expect("reentrant USB probe should not deadlock");
    }
    platform.register(ReentrantDevice);
    Ok(())
}

static USB_REENTRANT_REGISTER: DriverRegister = DriverRegister {
    name: "usb reentrant test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Usb {
        ids: &[CP210X_EA60],
        classes: &[],
        on_probe: probe_reentrant,
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

#[test]
fn usb_probe_callback_can_reenter_probe_registry() {
    rdrive::init(Platform::Static).expect("static platform should init");
    rdrive::register_add(USB_REENTRANT_REGISTER.clone());

    let host = rdrive::DeviceId::new();
    let device = UsbDevice::new(
        UsbDeviceKey::new(host, 7),
        1,
        2,
        device_descriptor(0x10c4, 0xea60),
    );

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let ok = rdrive::sync_usb_devices_for_host(host, &[device], true).is_ok();
        tx.send(ok).expect("test receiver should remain alive");
    });

    assert!(
        rx.recv_timeout(Duration::from_secs(2))
            .expect("USB probe should not deadlock while callback reenters"),
        "USB probe should succeed"
    );
    handle.join().expect("USB probe thread should finish");

    assert_eq!(REENTER_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(get_list::<ReentrantDevice>().len(), 1);
}
