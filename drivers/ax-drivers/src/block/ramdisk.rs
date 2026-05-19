use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

use crate::bindings::block::PlatformDeviceBlock;

const BLOCK_SIZE: usize = 512;
const DEFAULT_SIZE: usize = 16 * 1024 * 1024;

pub const DEVICE_NAME: &str = "ramdisk";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static Ramdisk",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_ramdisk,
    }],
};

fn probe_ramdisk(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }

    let blocks = DEFAULT_SIZE / BLOCK_SIZE;
    plat_dev.register_block(ramdisk::RamDisk::with_name(DEVICE_NAME, BLOCK_SIZE, blocks));
    log::info!("registered static ramdisk: {} bytes", DEFAULT_SIZE);
    Ok(())
}
