use ramdisk::RamDisk;
use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

use crate::static_devices::dma::IDENTITY_DMA;

pub(super) const DEVICE_NAME: &str = "ramdisk";

pub(super) const REGISTER: DriverRegister = DriverRegister {
    name: "Static RamDisk",
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

    let dev = rd_block::Block::new(RamDisk::new(512, 0x8000), &IDENTITY_DMA);
    plat_dev.register(dev);
    Ok(())
}
