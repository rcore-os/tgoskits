use aarch64_sysreg::SystemRegType;
use axaddrspace::device::{AccessWidth, DeviceAddrRange, SysRegAddr, SysRegAddrRange};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axerrno::AxResult;
use log::info;

impl BaseDeviceOps<SysRegAddrRange> for SysCntpCtlEl0 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> SysRegAddrRange {
        SysRegAddrRange {
            start: SysRegAddr::new(SystemRegType::CNTP_CTL_EL0 as usize),
            end: SysRegAddr::new(SystemRegType::CNTP_CTL_EL0 as usize),
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
        Ok(())
    }
}

pub struct SysCntpCtlEl0 {
    // Fields
}

impl SysCntpCtlEl0 {
    pub fn new() -> Self {
        Self {
            // Initialize fields
        }
    }
}
