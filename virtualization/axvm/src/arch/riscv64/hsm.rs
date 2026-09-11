//! RISC-V HSM exit handling through the VM-owned hart topology.

use axvm_types::GuestPhysAddr;

use super::{AxvmRiscvVcpu, RiscvDeferredRunWork};
use crate::{
    AxVmResult,
    architecture::{BoundVcpuExit, VcpuRunAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HartStart {
    pub(crate) target_cpu: u64,
    pub(crate) entry_point: GuestPhysAddr,
    pub(crate) arg: u64,
}

pub(super) fn target_vcpu_id(vm: &crate::AxVMRef, hart: usize) -> Option<usize> {
    vm.get_vcpu_affinities_pcpu_ids()
        .into_iter()
        .find_map(|(vcpu_id, _, configured_hart)| (configured_hart == hart).then_some(vcpu_id))
}

pub(crate) fn handle(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    exit: HartStart,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    info!(
        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{}] entry_point={:x} arg={:#x}",
        exit.target_cpu, exit.entry_point, exit.arg
    );

    let Some(target_vcpu_id) = usize::try_from(exit.target_cpu)
        .ok()
        .and_then(|hart| target_vcpu_id(vm, hart))
    else {
        warn!(
            "VM[{vm_id}] cannot resolve architecture CPU target {} to a VM-local vCPU",
            exit.target_cpu
        );
        vcpu.set_return_value(usize::MAX);
        return Ok(BoundVcpuExit::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        }));
    };

    match crate::runtime::vcpus::vcpu_on(
        vm.clone(),
        target_vcpu_id,
        exit.entry_point,
        exit.arg as _,
    ) {
        Ok(()) => vcpu.set_gpr(ax_cpu::registers::GprIndex::A0 as usize, 0),
        Err(err) => {
            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
            vcpu.set_return_value(usize::MAX);
        }
    }
    Ok(BoundVcpuExit::Complete(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
        resets_vm: false,
        exits_vcpu: false,
    }))
}
