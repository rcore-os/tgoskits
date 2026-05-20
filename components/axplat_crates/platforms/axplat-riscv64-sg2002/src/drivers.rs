use ax_drivers::block::cvsd;
use rdrive::{
    Platform, PlatformDevice,
    probe::{OnProbeError, static_::StaticDeviceDesc},
};

use crate::config::devices;

mod registers;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new(cvsd::DEVICE_NAME)];

pub(super) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    registers::append_linker_registers();
    register_cvsd();
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
}

fn register_cvsd() {
    match cvsd::register_mmio(
        static_descriptor(cvsd::DEVICE_NAME),
        devices::CVSD_PADDR,
        0x1000,
        devices::SYSCON_PADDR,
        0x8000,
    ) {
        Ok(()) | Err(OnProbeError::NotMatch) => {}
        Err(err) => panic!("failed to register CVSD: {err:?}"),
    }
}

fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
