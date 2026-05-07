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
    rk3588_reset_deassert,
};
