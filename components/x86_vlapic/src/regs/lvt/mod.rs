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

//! Local Vector Table

mod cmci;
mod error;
mod lint0;
mod lint1;
mod perfmon;
mod thermal;
mod timer;

pub use cmci::*;
pub use error::*;
pub use lint0::*;
pub use lint1::*;
pub use perfmon::*;
pub use thermal::*;
pub use timer::*;

pub use crate::consts::RESET_LVT_REG;

/// A read-write copy of LVT registers.
pub struct LocalVectorTable {
    /// LVT CMCI Register (FEE0 02F0H)
    pub lvt_cmci: LvtCmciRegisterLocal,
    /// LVT Timer Register (FEE0 0320H)
    pub lvt_timer: LvtTimerRegisterLocal,
    /// LVT Thermal Monitor Register (FEE0 0330H)
    pub lvt_thermal: LvtThermalMonitorRegisterLocal,
    /// LVT Performance Counter Register (FEE0 0340H)
    pub lvt_perf_count: LvtPerformanceCounterRegisterLocal,
    /// LVT LINT0 Register (FEE0 0350H)
    pub lvt_lint0: LvtLint0RegisterLocal,
    /// LVT LINT1 Register (FEE0 0360H)
    pub lvt_lint1: LvtLint1RegisterLocal,
    /// LVT Error register 0x37.
    pub lvt_err: LvtErrorRegisterLocal,
}

impl Default for LocalVectorTable {
    fn default() -> Self {
        LocalVectorTable {
            lvt_cmci: LvtCmciRegisterLocal::new(RESET_LVT_REG),
            lvt_timer: LvtTimerRegisterLocal::new(RESET_LVT_REG),
            lvt_thermal: LvtThermalMonitorRegisterLocal::new(RESET_LVT_REG),
            lvt_perf_count: LvtPerformanceCounterRegisterLocal::new(RESET_LVT_REG),
            lvt_lint0: LvtLint0RegisterLocal::new(RESET_LVT_REG),
            lvt_lint1: LvtLint1RegisterLocal::new(RESET_LVT_REG),
            lvt_err: LvtErrorRegisterLocal::new(RESET_LVT_REG),
        }
    }
}
