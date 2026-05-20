use ax_drivers::block::sdmmc;
use rdrive::{
    Platform, PlatformDevice,
    probe::{OnProbeError, static_::StaticDeviceDesc},
};

use crate::config::devices;

mod registers;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new(sdmmc::DEVICE_NAME)];

pub(super) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    registers::append_linker_registers();
    register_sdmmc();
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
}

fn register_sdmmc() {
    match sdmmc::register_mmio(
        static_descriptor(sdmmc::DEVICE_NAME),
        devices::SDMMC_PADDR,
        0x1000,
    ) {
        Ok(()) | Err(OnProbeError::NotMatch) => {}
        Err(err) => panic!("failed to register SD/MMC: {err:?}"),
    }
}

fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
