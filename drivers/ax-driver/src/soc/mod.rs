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

#[cfg(feature = "pinctrl")]
mod fixed_regulator;
#[cfg(feature = "rockchip-soc")]
pub(crate) mod rockchip;
#[cfg(any(feature = "rockchip-dwmmc", feature = "rk3588-cpufreq"))]
pub mod scmi;
#[cfg(feature = "starfive-soc")]
mod starfive;

#[cfg(feature = "rockchip-soc")]
pub use rockchip::{RockchipFdtPinctrlParser, RockchipPinCtrl};

#[cfg(not(feature = "rockchip-soc"))]
pub struct RockchipPinCtrl;

#[cfg(not(feature = "rockchip-soc"))]
pub struct RockchipFdtPinctrlParser;

#[cfg(not(feature = "rockchip-soc"))]
impl rdrive::DriverGeneric for RockchipPinCtrl {
    fn name(&self) -> &str {
        "rk3588-pinctrl-unavailable"
    }
}

#[cfg(not(feature = "rockchip-soc"))]
impl RockchipPinCtrl {
    pub fn enable_fixed_regulator(
        &mut self,
        phandle: fdt_edit::Phandle,
    ) -> Result<(), rdrive::probe::OnProbeError> {
        Err(rdrive::probe::OnProbeError::other(alloc::format!(
            "RK3588 pinctrl support is not enabled for regulator phandle {phandle:?}"
        )))
    }
}
