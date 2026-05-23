use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};
use rockchip_soc::{Cru, SocType};

use super::ClkDrv;
use crate::drivers::iomap;

const RK3588_CRU_GRF_BASE: usize = 0xfd5b_0000;
const RK3588_CRU_GRF_SIZE: usize = 0x1000;

module_driver!(
    name: "Rockchip RK3588 CRU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-cru"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let grf_phandle = info
        .node
        .as_node()
        .get_property("rockchip,grf")
        .and_then(|prop| prop.get_u32())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no rockchip,grf",
            info.node.name()
        )))?;

    info!(
        "RK3588 CRU reg: addr={:#x}, size={:#x}, rockchip,grf=<{:#x}>",
        base_reg.address as usize,
        base_reg.size.unwrap_or(0),
        grf_phandle
    );

    let mmio_base = iomap(
        (base_reg.address as usize).into(),
        base_reg.size.unwrap_or(0x5c000) as usize,
    )?;
    let grf_base = iomap(RK3588_CRU_GRF_BASE.into(), RK3588_CRU_GRF_SIZE)?;

    let cru = Cru::new(SocType::Rk3588, mmio_base, grf_base);
    plat_dev.register(rdif_clk::Clk::new(ClkDrv::new("rk3588-cru", cru)));
    info!("RK3588 CRU clock registered successfully");
    Ok(())
}
