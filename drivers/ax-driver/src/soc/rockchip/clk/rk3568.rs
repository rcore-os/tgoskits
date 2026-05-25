use log::info;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use rockchip_soc::{Cru, SocType};

use super::ClkDrv;
use crate::mmio::iomap;

const RK3568_CRU_GRF_BASE: usize = 0xfdc6_0000;
const RK3568_CRU_GRF_SIZE: usize = 0x10000;

module_driver!(
    name: "Rockchip RK3568 CRU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3568-cru"],
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
        "RK3568 CRU reg: addr={:#x}, size={:#x}, rockchip,grf=<{:#x}>",
        base_reg.address as usize,
        base_reg.size.unwrap_or(0),
        grf_phandle
    );

    let mmio_base = iomap(
        base_reg.address as usize,
        base_reg.size.unwrap_or(0x1000) as usize,
    )?;
    let grf_base = iomap(RK3568_CRU_GRF_BASE, RK3568_CRU_GRF_SIZE)?;

    let mut cru = Cru::new(SocType::Rk3568, mmio_base, grf_base);
    if let Cru::Rk3568(ref mut rk3568) = cru {
        rk3568.init_emmc();
    }
    plat_dev.register(rdif_clk::Clk::new(ClkDrv::new("rk3568-cru", cru)));
    info!("RK3568 CRU clock registered successfully");
    Ok(())
}
