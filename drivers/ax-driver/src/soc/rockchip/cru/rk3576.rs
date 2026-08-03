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

use ax_kspin::SpinRaw as Mutex;
use log::info;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use rockchip_soc::{Cru, SocType};

use super::{ClkDrv, ResetDrv};
use crate::mmio::iomap;

const RK3576_PMU0_GRF_BASE: usize = 0x2602_4000;
const RK3576_PMU0_GRF_SIZE: usize = 0x1000;

crate::model_register!(
    name: "Rockchip RK3576 CRU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3576-cru"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let base_reg =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;

    info!(
        "RK3576 CRU reg: addr={:#x}, size={:#x}",
        base_reg.address as usize,
        base_reg.size.unwrap_or(0)
    );

    let mmio_base = iomap(
        base_reg.address as usize,
        base_reg.size.unwrap_or(0x50000) as usize,
    )?;
    let grf_base = iomap(RK3576_PMU0_GRF_BASE, RK3576_PMU0_GRF_SIZE)?;
    let cru = alloc::sync::Arc::new(Mutex::new(Cru::new(SocType::Rk3576, mmio_base, grf_base)));

    plat_dev.register(rdif_reset::Reset::new(ResetDrv::new(
        "rk3576-cru-reset",
        cru.clone(),
    )));
    plat_dev.register(rdif_clk::Clk::new(ClkDrv::new("rk3576-cru-clock", cru)));
    info!("RK3576 CRU clock/reset registered successfully");
    Ok(())
}
