//! Shared secondary-vCPU boot flow for architectures that expose CPU-up exits.

use alloc::format;

use axvm_types::{GuestPhysAddr, VmArchVcpuOps, VmVcpuState};

use crate::{
    AxVmResult,
    architecture::{ArchOps, BoundVcpuExit, VcpuRunAction},
    ax_err_type,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuUpExit {
    pub(crate) target_cpu: u64,
    pub(crate) entry_point: GuestPhysAddr,
    pub(crate) arg: u64,
}

pub(crate) trait CpuUpOps: ArchOps {
    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        vcpu.set_gpr(0, 0);
    }

    fn target_vcpu_id(vm: &crate::AxVMRef, target_cpu: u64) -> Option<usize> {
        vm.get_vcpu_affinities_pcpu_ids()
            .iter()
            .find_map(|(vcpu_id, _, phys_id)| (*phys_id == target_cpu as usize).then_some(*vcpu_id))
    }
}

pub(crate) fn handle<A>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: CpuUpExit,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>>
where
    A: CpuUpOps<VCpu = crate::arch::ArchVCpu>,
{
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
        return Ok(BoundVcpuExit::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        }));
    };

    match start_vcpu::<A>(vm, target_vcpu_id, exit.entry_point, exit.arg as _) {
        Ok(()) => A::set_cpu_up_success(vcpu),
        Err(err) => {
            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
            vcpu.set_return_value(usize::MAX);
        }
    }
    Ok(BoundVcpuExit::Complete(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    }))
}

fn start_vcpu<A>(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> AxVmResult
where
    A: CpuUpOps<VCpu = crate::arch::ArchVCpu>,
{
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} not found")))?;
    if vcpu.state() != VmVcpuState::Free {
        return Err(ax_err_type!(
            BadState,
            format!("vCPU {} invalid state {:?}", vcpu.id(), vcpu.state())
        ));
    }

    vcpu.get_arch_vcpu()
        .set_entry(entry_point)
        .map_err(|error| crate::vcpu::map_vcpu_backend_error("set vCPU entry", error))?;
    A::set_vcpu_on_args(&vcpu, vcpu_id, arg);

    let task = crate::host::task::spawn_task(crate::runtime::vcpus::build_vcpu_task(vm, vcpu));
    vm.with_runtime(|runtime| {
        runtime.add_vcpu_task(vcpu_id, task);
        Ok(())
    })
}
