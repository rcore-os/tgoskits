use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};
use rockchip_pm::{RkBoard, RockchipPM};

use crate::iomap;

module_driver!(
    name: "Rockchip Pm",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-pmu"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let board = RkBoard::Rk3588;

    let mmio_base = iomap(base_reg.address, mmio_size)?;

    let pm = RockchipPM::new(mmio_base, board);

    plat_dev.register(pm);
    info!("Rockchip power manager registered successfully");
    Ok(())
}
