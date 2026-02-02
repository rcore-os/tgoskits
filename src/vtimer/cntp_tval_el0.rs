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

use aarch64_sysreg::SystemRegType;
use axaddrspace::device::{AccessWidth, DeviceAddrRange, SysRegAddr, SysRegAddrRange};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axerrno::AxResult;
use axvisor_api::time::{current_time_nanos, register_timer};
use log::info;

use alloc::boxed::Box;
use core::time::Duration;

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
    ) -> AxResult<usize> {
        todo!()
    }

    fn handle_write(
        &self,
        addr: <SysRegAddrRange as DeviceAddrRange>::Addr,
        _width: AccessWidth,
        val: usize,
    ) -> AxResult {
        info!("Write to emulator register: {addr:?}, value: {val}");
        let now = current_time_nanos();
        info!("Current time: {}, deadline: {}", now, now + val as u64);
        register_timer(
            Duration::from_nanos(now + val as u64),
            Box::new(|_| {
                crate::api_reexp::hardware_inject_virtual_interrupt(30);
            }),
        );
        Ok(())
    }
}

/// System register emulation for CNTP_TVAL_EL0.
///
/// Provides virtualization support for the physical timer value register.
#[derive(Default)]
pub struct SysCntpTvalEl0 {
    // Fields
}

impl SysCntpTvalEl0 {
    /// Creates a new CNTP_TVAL_EL0 register emulator.
    pub fn new() -> Self {
        Self {
            // Initialize fields
        }
    }
}
