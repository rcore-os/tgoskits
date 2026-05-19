use alloc::format;

use ax_driver_block::sdmmc::SdMmcDriver;
use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

pub const DEVICE_NAME: &str = "sdmmc";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static SD/MMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_sdmmc,
    }],
};

fn probe_sdmmc(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME || ax_config::devices::SDMMC_PADDR == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let mmio = axklib::mmio::ioremap_raw(ax_config::devices::SDMMC_PADDR.into(), 0x1000)
        .map_err(|err| OnProbeError::other(format!("failed to map SD/MMC: {err:?}")))?;
    let driver = unsafe { SdMmcDriver::new(mmio.as_ptr() as usize) };
    super::register_block(plat_dev, driver);
    Ok(())
}
