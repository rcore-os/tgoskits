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

#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuUpWork {
    target_vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
}

#[derive(Debug)]
pub(crate) enum PreparedCpuUp {
    Complete(VcpuRunAction),
    Defer(CpuUpWork),
}

/// Resolves architecture-visible CPU IDs through the VM-owned topology.
pub(crate) trait VmArchCpuIdResolver {
    fn vcpu_id_for_arch_cpu_id(&self, arch_cpu_id: usize) -> Option<usize>;
}

impl VmArchCpuIdResolver for crate::AxVM {
    fn vcpu_id_for_arch_cpu_id(&self, arch_cpu_id: usize) -> Option<usize> {
        self.get_vcpu_affinities_pcpu_ids().into_iter().find_map(
            |(vcpu_id, _, configured_cpu_id)| (configured_cpu_id == arch_cpu_id).then_some(vcpu_id),
        )
    }
}

pub(crate) trait CpuUpOps: ArchOps {
    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        vcpu.set_gpr(0, 0);
    }

    fn target_vcpu_id(vm: &crate::AxVMRef, target_cpu: u64) -> Option<usize> {
        usize::try_from(target_cpu)
            .ok()
            .and_then(|arch_cpu_id| vm.vcpu_id_for_arch_cpu_id(arch_cpu_id))
    }
}

pub(crate) fn prepare<A: CpuUpOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: CpuUpExit,
) -> AxVmResult<PreparedCpuUp> {
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
        return Ok(PreparedCpuUp::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        }));
    };

    Ok(PreparedCpuUp::Defer(CpuUpWork {
        target_vcpu_id,
        entry_point: exit.entry_point,
        arg: exit.arg as usize,
    }))
}

pub(crate) fn finish<A: CpuUpOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    work: CpuUpWork,
) -> VcpuRunAction {
    let vm_id = vm.id();
    match crate::runtime::vcpus::vcpu_on(
        vm.clone(),
        work.target_vcpu_id,
        work.entry_point,
        work.arg,
    ) {
        Ok(()) => A::set_cpu_up_success(vcpu),
        Err(err) => {
            warn!(
                "Failed to boot VM[{vm_id}] VCpu[{}]: {err:?}",
                work.target_vcpu_id
            );
            vcpu.set_return_value(usize::MAX);
        }
    }
    VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
        resets_vm: false,
        exits_vcpu: false,
    }
}
