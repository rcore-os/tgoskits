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

use rdrive::{
    DriverGeneric, KError, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use rockchip_soc::{ClkId, Cru, CruOp, SocType};

use crate::drivers::iomap;

const RK3588_CRU_GRF_BASE: usize = 0xfd5b_0000;
const RK3588_CRU_GRF_SIZE: usize = 0x1000;

module_driver!(
    name: "Rockchip CRU",
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
    let clk = rdif_clk::Clk::new(ClkDrv::new(cru));

    plat_dev.register(clk);
    info!("RK3588 CRU clock registered successfully");
    Ok(())
}

pub struct ClkDrv {
    inner: Cru,
}

impl ClkDrv {
    pub const fn new(cru: Cru) -> Self {
        Self { inner: cru }
    }
}

unsafe impl Send for ClkDrv {}

impl DriverGeneric for ClkDrv {
    fn name(&self) -> &str {
        "rk3588-cru"
    }
}

impl rdif_clk::Interface for ClkDrv {
    fn perper_enable(&mut self) {}

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, KError> {
        self.inner
            .clk_get_rate(clock_id(id))
            .map_err(|_| KError::InvalidArg { name: "clock_id" })
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        self.inner
            .clk_set_rate(clock_id(id), rate)
            .map_err(|_| KError::InvalidArg { name: "clock_id" })?;
        Ok(())
    }
}

fn clock_id(id: rdif_clk::ClockId) -> ClkId {
    let id: usize = id.into();
    ClkId::from(id)
}
