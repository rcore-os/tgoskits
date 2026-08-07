//! RISC-V guest IPI routing through the VM runtime interrupt channel.

use std::vec::Vec;

use riscv_vcpu::{RiscvIpiCompletion, RiscvIpiRequest};

use super::{AxvmRiscvVcpu, RiscvDeferredRunWork};
use crate::{
    AxVMRef, AxVmResult, InterruptTriggerMode,
    architecture::{BoundVcpuExit, cpu_up::VmArchCpuIdResolver},
    irq::{
        model::{PendingVcpuInterrupt, VirtualInterruptId},
        sender::VmInterruptSender,
    },
    vm::AxVCpuRef,
};

const SUPERVISOR_SOFTWARE_INTERRUPT_ID: VirtualInterruptId = VirtualInterruptId(1);

pub(super) fn handle(
    vm: &AxVMRef,
    vcpu: &AxVCpuRef<AxvmRiscvVcpu>,
    request: RiscvIpiRequest,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let completion = match resolve_targets(vm, request) {
        Ok(targets) => deliver(vm, &targets),
        Err(error) => {
            warn!(
                "VM[{}] VCpu[{}] rejected SBI IPI request {:?}: {:?}",
                vm.id(),
                vcpu.id(),
                request,
                error
            );
            RiscvIpiCompletion::InvalidParameter
        }
    };
    vcpu.get_arch_vcpu().complete_ipi(request, completion);
    Ok(BoundVcpuExit::Continue)
}

fn deliver(vm: &AxVMRef, targets: &[usize]) -> RiscvIpiCompletion {
    let sender = VmInterruptSender::new(vm);
    let interrupt = PendingVcpuInterrupt {
        id: SUPERVISOR_SOFTWARE_INTERRUPT_ID,
        trigger: InterruptTriggerMode::LevelTriggered,
    };

    for &target_vcpu_id in targets {
        if let Err(error) = sender.send(target_vcpu_id, interrupt) {
            warn!(
                "VM[{}] failed to deliver SBI IPI to VCpu[{}]: {:?}",
                vm.id(),
                target_vcpu_id,
                error
            );
            return RiscvIpiCompletion::Failed;
        }
    }
    RiscvIpiCompletion::Success
}

fn resolve_targets(vm: &AxVMRef, request: RiscvIpiRequest) -> Result<Vec<usize>, IpiTargetError> {
    if request.hart_mask_base() == usize::MAX {
        return Ok(vm.vcpu_list().iter().map(|vcpu| vcpu.id()).collect());
    }

    let mut targets = Vec::new();
    for bit in 0..usize::BITS {
        if request.hart_mask() & (1usize << bit) == 0 {
            continue;
        }
        let hart_id = request
            .hart_mask_base()
            .checked_add(bit as usize)
            .ok_or(IpiTargetError::HartIdOverflow)?;
        let target_vcpu_id = vm
            .vcpu_id_for_arch_cpu_id(hart_id)
            .ok_or(IpiTargetError::UnavailableHart(hart_id))?;
        if targets.contains(&target_vcpu_id) {
            return Err(IpiTargetError::DuplicateVcpu(target_vcpu_id));
        }
        targets.push(target_vcpu_id);
    }
    Ok(targets)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IpiTargetError {
    HartIdOverflow,
    UnavailableHart(usize),
    DuplicateVcpu(usize),
}
