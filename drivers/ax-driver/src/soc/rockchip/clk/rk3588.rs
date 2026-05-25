// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use log::info;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use rockchip_soc::{Cru, SocType};

use super::ClkDrv;
use crate::mmio::iomap;

const RK3588_CRU_GRF_BASE: usize = 0xfd5b_0000;
const RK3588_CRU_GRF_SIZE: usize = 0x1000;

crate::model_register!(
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
        base_reg.address as usize,
        base_reg.size.unwrap_or(0x5c000) as usize,
    )?;
    let grf_base = iomap(RK3588_CRU_GRF_BASE, RK3588_CRU_GRF_SIZE)?;

    let cru = Cru::new(SocType::Rk3588, mmio_base, grf_base);
    plat_dev.register(rdif_clk::Clk::new(ClkDrv::new("rk3588-cru", cru)));
    info!("RK3588 CRU clock registered successfully");
    Ok(())
}
