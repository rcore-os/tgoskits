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

mod rockchip;

#[cfg(all(feature = "rockchip-soc", not(feature = "rk3568-clk")))]
pub(crate) use rockchip::{
    RockchipPinCtrl, rk3588_enable_clock, rk3588_enable_power_domain, rk3588_reset_assert,
    rk3588_reset_deassert, rk3588_set_clock_rate,
};

#[cfg(not(all(feature = "rockchip-soc", not(feature = "rk3568-clk"))))]
pub(crate) fn rk3588_enable_clock(id: u32) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::other(alloc::format!(
        "RK3588 clock support is not enabled for clock {id:#x}"
    )))
}

#[cfg(not(all(feature = "rockchip-soc", not(feature = "rk3568-clk"))))]
pub(crate) fn rk3588_set_clock_rate(
    id: u32,
    rate_hz: u64,
) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::other(alloc::format!(
        "RK3588 clock support is not enabled for clock {id:#x} rate {rate_hz}"
    )))
}

#[cfg(not(all(feature = "rockchip-soc", not(feature = "rk3568-clk"))))]
pub(crate) fn rk3588_enable_power_domain(domain: usize) -> Result<(), alloc::string::String> {
    Err(alloc::format!(
        "RK3588 power-domain support is not enabled for power domain {domain}"
    ))
}

#[cfg(not(all(feature = "rockchip-soc", not(feature = "rk3568-clk"))))]
pub(crate) fn rk3588_reset_deassert(id: u64) -> Result<(), rdrive::probe::OnProbeError> {
    Err(rdrive::probe::OnProbeError::other(alloc::format!(
        "RK3588 reset support is not enabled for reset {id:#x}"
    )))
}
