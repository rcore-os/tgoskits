use ax_driver_block::ramdisk::RamDisk;
use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

pub const DEVICE_NAME: &str = "ramdisk";

pub const REGISTER: DriverRegister = DriverRegister {
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

    super::register_block(plat_dev, RamDisk::new(0x100_0000));
    Ok(())
}
