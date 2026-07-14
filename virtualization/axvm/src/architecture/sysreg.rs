//! Shared system-register device exits used by AArch64 and x86_64 guests.

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
    let val = vm
        .get_devices()?
        .handle_sys_reg_read(exit.addr, AccessWidth::Qword)
        .map_err(|error| AxVmError::device("read guest system register", error))?;
    vcpu.set_gpr(exit.reg, val);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_write<D>(
    vm: &crate::AxVM,
    exit: SysRegWriteExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    vm.get_devices()?
        .handle_sys_reg_write(exit.addr, AccessWidth::Qword, exit.value as usize)
        .map_err(|error| AxVmError::device("write guest system register", error))?;
    Ok(BoundVcpuExit::Continue)
}
