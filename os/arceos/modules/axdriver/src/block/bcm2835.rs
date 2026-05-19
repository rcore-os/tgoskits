use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

pub const DEVICE_NAME: &str = "bcm2835-sdhci";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static BCM2835 SDHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_bcm2835,
    }],
};

fn probe_bcm2835(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }

    let driver = ax_driver_block::bcm2835sdhci::SDHCIDriver::try_new()
        .map_err(|err| OnProbeError::other(alloc::format!("BCM2835 SDHCI init failed: {err:?}")))?;
    super::register_block(plat_dev, driver);
    Ok(())
}
