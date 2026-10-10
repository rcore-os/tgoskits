//! System-register device access belongs to the unloaded task layer.

use axdevice_base::{BusKind, DeviceAccess, DeviceVcpuId};
use axvm_types::{AccessWidth, SysRegAddr};

use super::ArchOps;
#[cfg(target_arch = "aarch64")]
use super::ops::RegisterCompletion;
use crate::{
    AxVmError, AxVmResult, engine::VcpuAction, runtime::hvc::GuestRequest, services::RunServices,
};

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SysRegReadExit {
    pub(crate) addr: SysRegAddr,
    pub(crate) reg: usize,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct SysRegWriteExit {
    pub(crate) addr: SysRegAddr,
    pub(crate) value: u64,
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn handle_read<A: ArchOps>(
    services: &RunServices,
    vcpu_id: usize,
    exit: SysRegReadExit,
) -> AxVmResult<VcpuAction<A::Completion, GuestRequest>> {
    let value = services
        .read_device(&sysreg_access(vcpu_id, exit.addr))?
        .ok_or_else(|| missing_sysreg_error("read", exit.addr))?;
    Ok(VcpuAction::Reenter(
        RegisterCompletion::Gpr {
            register: exit.reg,
            value: value as usize,
        }
        .into(),
    ))
}

pub(crate) fn handle_write<A: ArchOps>(
    services: &RunServices,
    vcpu_id: usize,
    exit: SysRegWriteExit,
) -> AxVmResult<VcpuAction<A::Completion, GuestRequest>> {
    if !services.write_device(&sysreg_access(vcpu_id, exit.addr), exit.value)? {
        return Err(missing_sysreg_error("write", exit.addr));
    }
    Ok(VcpuAction::Reenter(A::Completion::default()))
}

fn sysreg_access(vcpu_id: usize, addr: SysRegAddr) -> DeviceAccess {
    DeviceAccess::new(
        DeviceVcpuId::new(vcpu_id),
        BusKind::SysReg,
        addr.addr() as u64,
        AccessWidth::Qword,
    )
}

fn missing_sysreg_error(operation: &'static str, addr: SysRegAddr) -> AxVmError {
    AxVmError::device(
        "access guest system register",
        axdevice::DeviceManagerError::Access {
            operation,
            bus: BusKind::SysReg,
            addr: addr.addr() as u64,
            width: AccessWidth::Qword,
            source: axdevice_base::DeviceError::NotFound,
        },
    )
}
