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

extern crate alloc;

use axklib::mem::iomap;
use rdif_clk::{ClockId, Interface};
use rdrive::{DriverGeneric, KError};
use rk3568_clk::CRU;
use rk3568_clk::cru_clksel_con28_bits::*;

use rdrive::{PlatformDevice, probe::OnProbeError};
use rdrive::{module_driver, register::FdtInfo};

/// 频率常量
const MHZ: u32 = 1_000_000;
const KHZ: u32 = 1_000;

use core::convert::Into;
use core::result::Result::{self, *};
use log::{debug, info, warn};

pub struct ClkDriver(CRU);

pub const EMMC_CLK_ID: usize = 0x7c;

impl ClkDriver {
    pub fn new(cru_address: u64) -> Self {
        ClkDriver(CRU::new(cru_address as *mut _))
    }
}

impl DriverGeneric for ClkDriver {
    fn open(&mut self) -> Result<(), KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), KError> {
        Ok(())
    }
}

impl Interface for ClkDriver {
    fn perper_enable(&mut self) {
        debug!("perper_enable");
    }

    fn get_rate(&self, id: ClockId) -> Result<u64, KError> {
        let rate = match id.into() {
            EMMC_CLK_ID => {
                let con = self.0.cru_clksel_get_cclk_emmc();
                con >> CRU_CLKSEL_CCLK_EMMC_POS
            }
            _ => {
                warn!("Unsupported clock ID: {:?}", id);
                Err(KError::InvalidArg { name: "clock_id" })?
            }
        };
        Ok(rate as u64)
    }

    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError> {
        match id.into() {
            EMMC_CLK_ID => {
                info!("Setting eMMC clock to {} Hz", rate);
                let src_clk = match rate as u32 {
                    r if r == 24 * MHZ => CRU_CLKSEL_CCLK_EMMC_XIN_SOC0_MUX,
                    r if r == 52 * MHZ || r == 50 * MHZ => CRU_CLKSEL_CCLK_EMMC_CPL_DIV_50M,
                    r if r == 100 * MHZ => CRU_CLKSEL_CCLK_EMMC_CPL_DIV_100M,
                    r if r == 150 * MHZ => CRU_CLKSEL_CCLK_EMMC_GPL_DIV_150M,
                    r if r == 200 * MHZ => CRU_CLKSEL_CCLK_EMMC_GPL_DIV_200M,
                    r if r == 400 * KHZ || r == 375 * KHZ => CRU_CLKSEL_CCLK_EMMC_SOC0_375K,
                    _ => panic!("Unsupported eMMC clock rate: {} Hz", rate),
                };
                self.0.cru_clksel_set_cclk_emmc(src_clk);
            }
            _ => {
                warn!("Unsupported clock ID: {:?}", id);
                return Err(KError::InvalidArg { name: "clock_id" });
            }
        }
        Ok(())
    }
}

module_driver!(
    name: "Rockchip CRU",
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
    info!("Probing Rockchip RK3568 Clock...");

    let cru_reg = info
        .node
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    info!(
        "CRU reg: addr={:#x}, size={:#x}",
        cru_reg.address as usize,
        cru_reg.size.unwrap_or(0)
    );

    let cru_reg_base = iomap(
        (cru_reg.address as usize).into(),
        cru_reg.size.unwrap_or(0x1000),
    )
    .expect("Failed to iomap CRU");

    let cru_address = cru_reg_base.as_ptr() as u64;

    debug!("cru address: {:#x}", cru_address);

    let clk = rdif_clk::Clk::new(ClkDriver::new(cru_address));

    plat_dev.register(clk);

    Ok(())
}
