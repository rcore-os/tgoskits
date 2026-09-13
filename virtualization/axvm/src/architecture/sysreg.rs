//! Shared system-register device exits used by AArch64 and x86_64 guests.

use axdevice::DeviceManagerError;
use axdevice_base::{BusKind, DeviceAccess, DeviceError, DeviceVcpuId};
use axvm_types::{AccessWidth, SysRegAddr, VmArchVcpuOps};

use crate::{AxVmError, AxVmResult, architecture::BoundVcpuExit};

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

pub(crate) fn handle_read<V: VmArchVcpuOps, D>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: SysRegReadExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    let access = sysreg_access(vcpu.id(), exit.addr);
    let val = vm
        .get_devices()?
        .try_read(&access)
        .map_err(|error| AxVmError::device("read guest system register", error))?
        .ok_or_else(|| missing_sysreg_error("read", exit.addr))?;
    vcpu.set_gpr(exit.reg, val as usize);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_write<V: VmArchVcpuOps, D>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: SysRegWriteExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    let access = sysreg_access(vcpu.id(), exit.addr);
    if !vm.try_write_device(&access, exit.value)? {
        return Err(missing_sysreg_error("write", exit.addr));
    }
    Ok(BoundVcpuExit::Continue)
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
        DeviceManagerError::Access {
            operation,
            bus: BusKind::SysReg,
            addr: addr.addr() as u64,
            width: AccessWidth::Qword,
            source: DeviceError::NotFound,
        },
    )
}
