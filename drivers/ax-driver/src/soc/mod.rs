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
mod rockchip;
#[cfg(feature = "rockchip-dwmmc")]
pub mod scmi;

#[cfg(any(
    feature = "jpeg",
    feature = "rk3588-pcie",
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci",
    feature = "rockchip-sdhci"
))]
pub(crate) use rockchip::RockchipResetOps;
#[cfg(feature = "rockchip-soc")]
pub use rockchip::{
    RockchipFdtPinctrlParser, RockchipPinCtrl, rk3588_enable_clock, rk3588_enable_power_domain,
    rk3588_set_clock_rate,
};

#[cfg(not(feature = "rockchip-soc"))]
pub fn rk3588_enable_clock(id: u32) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::other(alloc::format!(
        "RK3588 clock support is not enabled for clock {id:#x}"
    )))
}

#[cfg(not(feature = "rockchip-soc"))]
pub fn rk3588_set_clock_rate(id: u32, rate_hz: u64) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::other(alloc::format!(
        "RK3588 clock support is not enabled for clock {id:#x} rate {rate_hz}"
    )))
}

#[cfg(not(feature = "rockchip-soc"))]
pub fn rk3588_enable_power_domain(domain: usize) -> Result<(), alloc::string::String> {
    Err(alloc::format!(
        "RK3588 power-domain support is not enabled for power domain {domain}"
    ))
}

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
