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
        info!("Write to emulator register: {:?}, value: {}", addr, val);
        let now = current_time_nanos();
        info!("Current time: {}, deadline: {}", now, now + val as u64);
        register_timer(
            Duration::from_nanos(now + val as u64),
            Box::new(|_| {
                axvisor_api::arch::hardware_inject_virtual_interrupt(30);
            }),
        );
        Ok(())
    }
}

pub struct SysCntpTvalEl0 {
    // Fields
}

impl SysCntpTvalEl0 {
    pub fn new() -> Self {
        Self {
            // Initialize fields
        }
    }
}
