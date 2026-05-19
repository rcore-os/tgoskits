use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
use {alloc::format, ax_driver_block::cvsd::CvsdDriver};

pub const DEVICE_NAME: &str = "cvsd";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static CV SD/MMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_cvsd,
    }],
};

fn probe_cvsd(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }
    probe_cvsd_target(plat_dev)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn probe_cvsd_target(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if ax_config::devices::CVSD_PADDR == 0 {
        return Err(OnProbeError::NotMatch);
    }

    log::info!(
        "Probe CV SD Bootable Part @ {:#x}",
        ax_config::devices::CVSD_PADDR
    );
    let sdmmc = map_region(ax_config::devices::CVSD_PADDR, 0x1000, "CVSD")?;
    let syscon = map_region(ax_config::devices::SYSCON_PADDR, 0x8000, "SYSCON")?;
    let driver = CvsdDriver::new(sdmmc, syscon)
        .map_err(|err| OnProbeError::other(format!("CVSD init failed: {err:?}")))?;
    super::register_block(plat_dev, driver);
    Ok(())
}

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
fn probe_cvsd_target(_plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    Err(OnProbeError::NotMatch)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn map_region(address: usize, size: usize, name: &str) -> Result<usize, OnProbeError> {
    let mmio = axklib::mmio::ioremap_raw(address.into(), size)
        .map_err(|err| OnProbeError::other(format!("failed to map {name}: {err:?}")))?;
    Ok(mmio.as_ptr() as usize)
}
