//! Architecture-neutral handlers for exits shared by every guest architecture.

use axvm_types::VmArchVcpuOps;

use super::{BoundVcpuExit, HypercallExit, VcpuRunAction};
use crate::{AxVmError, AxVmResult};

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
