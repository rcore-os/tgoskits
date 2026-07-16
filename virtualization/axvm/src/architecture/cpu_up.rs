//! Shared secondary-vCPU boot flow for architectures that expose CPU-up exits.

use axvm_types::GuestPhysAddr;

use crate::{
    AxVmResult,
    architecture::{ArchOps, VcpuRunAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuUpExit {
    pub(crate) target_cpu: u64,
    pub(crate) entry_point: GuestPhysAddr,
    pub(crate) arg: u64,
}

pub(crate) trait CpuUpOps: ArchOps {
    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        vcpu.set_gpr(0, 0);
    }

    fn target_vcpu_id(vm: &crate::AxVMRef, target_cpu: u64) -> Option<usize> {
        vm.get_vcpu_affinities_pcpu_ids()
            .iter()
            .find_map(|(vcpu_id, _, phys_id)| (*phys_id == target_cpu as usize).then_some(*vcpu_id))
    }
}

pub(crate) fn finish<A: CpuUpOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: CpuUpExit,
) -> AxVmResult<VcpuRunAction> {
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    info!(
        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{}] entry_point={:x} arg={:#x}",
        exit.target_cpu, exit.entry_point, exit.arg
    );

    let Some(target_vcpu_id) = A::target_vcpu_id(vm, exit.target_cpu) else {
        warn!(
            "VM[{vm_id}] cannot resolve architecture CPU target {} to a VM-local vCPU",
            exit.target_cpu
        );
        vcpu.set_return_value(usize::MAX);
        return Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        });
    };

    match crate::runtime::vcpus::vcpu_on(
        vm.clone(),
        target_vcpu_id,
        exit.entry_point,
        exit.arg as _,
    ) {
        Ok(()) => A::set_cpu_up_success(vcpu),
        Err(err) => {
            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
            vcpu.set_return_value(usize::MAX);
        }
    }
    Ok(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    })
}
