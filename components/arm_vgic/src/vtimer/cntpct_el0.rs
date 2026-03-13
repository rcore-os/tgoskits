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

use aarch64_cpu::registers::{Readable, CNTPCT_EL0};
use aarch64_sysreg::SystemRegType;
use axaddrspace::device::{AccessWidth, DeviceAddrRange, SysRegAddr, SysRegAddrRange};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axerrno::AxResult;
use log::info;

impl BaseDeviceOps<SysRegAddrRange> for SysCntpctEl0 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> SysRegAddrRange {
        SysRegAddrRange {
            start: SysRegAddr::new(SystemRegType::CNTPCT_EL0 as usize),
            end: SysRegAddr::new(SystemRegType::CNTPCT_EL0 as usize),
        }
    }

    fn handle_read(
        &self,
        _addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
    ) -> AxResult<usize> {
        Ok(CNTPCT_EL0.get() as usize)
    }

    fn handle_write(
        &self,
        addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
        val: usize,
    ) -> AxResult {
        info!("Write to emulator register: {addr:?}, value: {val}");
        Ok(())
    }
}

/// System register emulation for CNTPCT_EL0.
///
/// Provides virtualization support for the physical counter register.
#[derive(Default)]
pub struct SysCntpctEl0 {
    // Fields
}

impl SysCntpctEl0 {
    /// Creates a new CNTPCT_EL0 register emulator.
    pub fn new() -> Self {
        Self {
            // Initialize fields
        }
    }
}
