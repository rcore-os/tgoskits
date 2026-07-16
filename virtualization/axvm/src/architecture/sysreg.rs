//! Shared system-register exit capture used by AArch64 and x86_64 guests.
//!
//! This file is included only from those architecture modules. It deliberately
//! keeps the target-specific device protocol out of the common architecture
//! layer while preserving one deferred execution contract.

use axvm_types::{AccessWidth, SysRegAddr, VmArchVcpuOps};

use crate::{
    AxVmError, AxVmResult,
    architecture::{BoundVcpuExit, VcpuRunAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DeferredRunWork {
    Read(SysRegReadExit),
    Write(SysRegWriteExit),
}

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

pub(crate) fn handle_read<V, D>(
    _vm: &crate::AxVM,
    _vcpu: &crate::vm::AxVCpuRef<V>,
    exit: SysRegReadExit,
) -> AxVmResult<BoundVcpuExit<D>>
where
    V: VmArchVcpuOps,
    D: From<DeferredRunWork>,
{
    Ok(BoundVcpuExit::Defer(DeferredRunWork::Read(exit).into()))
}

pub(crate) fn handle_write<D>(
    _vm: &crate::AxVM,
    exit: SysRegWriteExit,
) -> AxVmResult<BoundVcpuExit<D>>
where
    D: From<DeferredRunWork>,
{
    Ok(BoundVcpuExit::Defer(DeferredRunWork::Write(exit).into()))
}

pub(crate) fn finish<V: VmArchVcpuOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    work: DeferredRunWork,
) -> AxVmResult<VcpuRunAction> {
    match work {
        DeferredRunWork::Read(exit) => {
            let value = vm
                .get_devices()?
                .handle_sys_reg_read(exit.addr, AccessWidth::Qword)
                .map_err(|error| AxVmError::device("read guest system register", error))?;
            vcpu.set_gpr(exit.reg, value);
        }
        DeferredRunWork::Write(exit) => {
            vm.get_devices()?
                .handle_sys_reg_write(exit.addr, AccessWidth::Qword, exit.value as usize)
                .map_err(|error| AxVmError::device("write guest system register", error))?;
        }
    }
    Ok(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    })
}
