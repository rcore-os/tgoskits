//! AArch64 guest SGI delivery through the VM-local GICv3 controller.

use super::Aarch64DeferredRunWork;
use crate::{AxVmResult, architecture::BoundVcpuExit};

#[derive(Clone, Copy, Debug)]
pub(crate) struct SendIpiExit {
    pub(crate) sgi1r: u64,
}

pub(crate) fn handle(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    exit: SendIpiExit,
) -> AxVmResult<BoundVcpuExit<Aarch64DeferredRunWork>> {
    debug!(
        "VM[{}] run VCpu[{vcpu_id}] ICC_SGI1R_EL1={:#x}",
        vm.id(),
        exit.sgi1r
    );
    let controller = vm.with_resources_mut(|resources| {
        resources.arch_state_mut().gic_controller().ok_or_else(|| {
            crate::ax_err_type!(BadState, "VM has no registered AArch64 GICv3 controller")
        })
    })?;
    controller
        .write_sgi1r(arm_vgic::GicVcpuId::new(vcpu_id), exit.sgi1r)
        .map_err(|error| crate::AxVmError::interrupt("send GICv3 SGI", error))?;
    Ok(BoundVcpuExit::Complete(
        crate::architecture::VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        },
    ))
}
