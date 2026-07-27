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

use super::cntp_timer::CntpTimerState;

impl BaseDeviceOps<SysRegAddrRange> for SysCntpCvalEl0 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> SysRegAddrRange {
        SysRegAddrRange {
            start: SysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize),
            end: SysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize),
        }
    }

    fn handle_read(
        &self,
        _addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
    ) -> DeviceResult<usize> {
        Ok(self.state.read_cval() as usize)
    }

    fn handle_write(
        &self,
        _addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        self.state.write_cval(val as u64);
        Ok(())
    }
}

/// System register emulation for CNTP_CVAL_EL0.
///
/// CNTP_CVAL_EL0 is banked per processing element. The current AxVisor
/// vTimer device model is instantiated for a single-vCPU guest, so this
/// device preserves the guest-visible compare value for that vCPU.
pub struct SysCntpCvalEl0 {
    state: Arc<CntpTimerState>,
}

impl SysCntpCvalEl0 {
    /// Creates a new CNTP_CVAL_EL0 register emulator.
    pub fn new() -> Self {
        Self::from_state(Arc::new(CntpTimerState::new()))
    }

    pub(super) fn from_state(state: Arc<CntpTimerState>) -> Self {
        Self { state }
    }
}

impl Default for SysCntpCvalEl0 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_cntp_cval_address() {
        let device = SysCntpCvalEl0::new();
        let range = device.address_range();

        assert_eq!(
            range.start,
            SysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize)
        );
        assert_eq!(range.end, range.start);
    }

    #[test]
    fn preserves_guest_visible_value() {
        let device = SysCntpCvalEl0::new();
        let addr = SysRegAddr::new(SystemRegType::CNTP_CVAL_EL0 as usize);
        let value = 0x1234_5678_9abc_def0usize;

        device
            .handle_write(addr, AccessWidth::Qword, value)
            .unwrap();

        assert_eq!(device.handle_read(addr, AccessWidth::Qword).unwrap(), value);
    }
}
