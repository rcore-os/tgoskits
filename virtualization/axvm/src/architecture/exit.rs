//! Architecture-neutral handlers for exits shared by every guest architecture.

use axvm_types::VmArchVcpuOps;

use super::{ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
use crate::{AxVmError, AxVmResult};

pub(crate) fn handle_mmio_read<V: VmArchVcpuOps, D>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: MmioReadExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    let raw = vm
        .get_devices()?
        .handle_mmio_read(exit.addr, exit.width)
        .map_err(|error| AxVmError::device("read guest MMIO", error))?;
    let masked = raw & crate::vm::width_mask(exit.width);
    let val = if exit.signed_ext {
        crate::vm::sign_extend_value(masked, exit.width)
    } else {
        masked & crate::vm::width_mask(exit.reg_width)
    };
    vcpu.set_gpr(exit.reg, val);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_mmio_write<A: ArchOps>(
    vm: &crate::AxVMRef,
    exit: MmioWriteExit,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
    vm.handle_mmio_write(exit.addr, exit.width, exit.data as usize)?;
    A::after_mmio_write(vm);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_hypercall<V: VmArchVcpuOps, D>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: HypercallExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    debug!("Hypercall [{:#x}] args {:x?}", exit.nr, exit.args);
    match crate::runtime::hvc::HyperCall::new(vm.clone(), exit.nr, exit.args) {
        Ok(hypercall) => {
            let ret_val = match hypercall.execute() {
                Ok(ret_val) => ret_val as isize,
                Err(error) => {
                    let err = AxVmError::from(error);
                    warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
                    -1
                }
            };
            vcpu.set_return_value(ret_val as usize);
        }
        Err(error) => {
            let err = AxVmError::from(error);
            warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
        }
    }
    Ok(BoundVcpuExit::Complete(VcpuRunAction::resume()))
}
