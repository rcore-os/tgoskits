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
    AccessWidth, BaseDeviceOps, DeviceAddrRange, DeviceResult, EmuDeviceType, SysRegAddr,
    SysRegAddrRange,
};
use log::debug;

use crate::vtimer::{VtimerBackend, VtimerState};

impl BaseDeviceOps<SysRegAddrRange> for SysCntpTvalEl0 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> SysRegAddrRange {
        SysRegAddrRange {
            start: SysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize),
            end: SysRegAddr::new(SystemRegType::CNTP_TVAL_EL0 as usize),
        }
    }

    fn handle_read(
        &self,
        _addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
    ) -> DeviceResult<usize> {
        Ok(self.state.timer_value(self.backend.current_time_nanos()) as usize)
    }

    fn handle_write(
        &self,
        addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        debug!("Write to virtual timer register: {addr:?}, value: {val}");
        self.state
            .write_timer_value(val as u64, Arc::clone(&self.backend));
        Ok(())
    }
}

/// System register emulation for CNTP_TVAL_EL0.
///
/// Provides virtualization support for the physical timer value register.
pub struct SysCntpTvalEl0 {
    state: Arc<VtimerState>,
    backend: Arc<dyn VtimerBackend>,
}

impl SysCntpTvalEl0 {
    /// Creates a new CNTP_TVAL_EL0 register emulator.
    pub fn new(state: Arc<VtimerState>, backend: Arc<dyn VtimerBackend>) -> Self {
        Self { state, backend }
    }
}
