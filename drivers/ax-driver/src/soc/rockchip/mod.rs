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

#[cfg(feature = "rockchip-soc")]
mod cru;

#[cfg(feature = "rockchip-pm")]
mod pm;

#[cfg(feature = "rockchip-soc")]
mod pinctrl;

#[cfg(feature = "rockchip-soc")]
pub use cru::{rk3588_enable_clock, rk3588_set_clock_rate};
#[cfg(feature = "rockchip-soc")]
pub use pinctrl::{RockchipFdtPinctrlParser, RockchipPinCtrl};
#[cfg(all(feature = "rockchip-soc", feature = "rockchip-pm"))]
pub use pm::rk3588_enable_power_domain;

#[cfg(all(feature = "rockchip-soc", not(feature = "rockchip-pm")))]
pub fn rk3588_enable_power_domain(domain: usize) -> Result<(), alloc::string::String> {
    Err(alloc::format!(
        "rockchip-pm feature is not enabled for power domain {domain}"
    ))
}
