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

use alloc::sync::Arc;

use aarch64_sysreg::SystemRegType;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError, DeviceResult,
    Resource,
};
use log::debug;

use super::cntp_timer::CntpTimerState;

const CNTP_CVAL_EL0_ADDR: u32 = SystemRegType::CNTP_CVAL_EL0 as u32;

impl SysCntpCvalEl0 {
    /// Reads CNTP_CVAL_EL0.
    pub fn read_register(&self, _width: AccessWidth) -> DeviceResult<usize> {
        Ok(self.state.read_cval() as usize)
    }

    /// Writes CNTP_CVAL_EL0.
    pub fn write_register(&self, _width: AccessWidth, val: usize) -> DeviceResult {
        debug!("Write to virtual timer register CNTP_CVAL_EL0, value: {val}");
        self.state.write_cval(val as u64);
        Ok(())
    }
}

/// System register emulation for CNTP_CVAL_EL0.
///
/// Provides virtualization support for the physical timer compare register.
pub struct SysCntpCvalEl0 {
    state: Arc<CntpTimerState>,
    resources: [Resource; 1],
}

impl SysCntpCvalEl0 {
    /// Creates a new CNTP_CVAL_EL0 register emulator.
    pub fn new() -> Self {
        Self::from_state(Arc::new(CntpTimerState::new()))
    }

    pub(super) fn from_state(state: Arc<CntpTimerState>) -> Self {
        Self {
            state,
            resources: [Resource::SysReg {
                addr: CNTP_CVAL_EL0_ADDR,
                count: 1,
            }],
        }
    }
}

impl Default for SysCntpCvalEl0 {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for SysCntpCvalEl0 {
    fn name(&self) -> &str {
        "aarch64-cntp-cval-el0"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::SysReg || access.addr != CNTP_CVAL_EL0_ADDR as u64 {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if access.is_read {
            self.read_register(access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            self.write_register(access.width, access.data as usize)
                .map(|_| BusResponse::Write)
        }
    }
}
