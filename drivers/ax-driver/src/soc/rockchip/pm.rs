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
use rockchip_pm::{PowerDomain, RkBoard, RockchipPM};

use crate::mmio::iomap;

crate::model_register!(
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
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let board = RkBoard::Rk3588;

    let mmio_base = iomap(base_reg.address as usize, mmio_size)?;
    let pm = RockchipPM::new(mmio_base, board);

    plat_dev.register(pm);
    info!("Rockchip power manager registered successfully");
    Ok(())
}

pub fn rk3588_enable_power_domain(domain: usize) -> Result<(), alloc::string::String> {
    let pm = rdrive::get_one::<RockchipPM>()
        .ok_or_else(|| alloc::format!("RockchipPM not found for power domain {domain}"))?;
    let mut pm = pm
        .lock()
        .map_err(|err| alloc::format!("failed to lock RockchipPM: {err}"))?;

    pm.power_domain_on(PowerDomain(domain))
        .map_err(|err| alloc::format!("{err:?}"))
}
