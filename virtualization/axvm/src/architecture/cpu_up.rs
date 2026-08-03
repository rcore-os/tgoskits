//! Shared secondary-vCPU boot flow for architectures that expose CPU-up exits.

use alloc::format;

use axvm_types::{GuestPhysAddr, VmArchVcpuOps, VmVcpuState};

use crate::{
    AxVmError, AxVmResult,
    arch::CurrentArch,
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
    fn set_vcpu_on_args(vcpu: &mut Self::VCpu, _vcpu_id: usize, arg: usize) {
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

pub(crate) fn handle<A: CpuUpOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: CpuUpExit,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
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

    match vcpu_on(vm.clone(), target_vcpu_id, exit.entry_point, exit.arg as _) {
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

impl<A: VmArchVcpuOps> crate::vcpu::AxVCpu<A> {
    fn reserve_startup(&self) -> AxVmResult {
        self.transition_state(VmVcpuState::Free, VmVcpuState::Starting)
    }

    fn cancel_startup(&self) -> AxVmResult {
        self.transition_state(VmVcpuState::Starting, VmVcpuState::Free)
    }
}

fn vcpu_on(
    vm: crate::runtime::VMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> AxVmResult {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} not found")))?;
    let runtime = vm.with_runtime(|runtime| Ok(runtime.clone()))?;
    runtime.reserve_vcpu_task(vcpu_id)?;
    if let Err(error) = vcpu.reserve_startup() {
        return Err(cancel_task_reservation_after_error(
            &runtime, vcpu_id, error,
        ));
    }
    if let Err(error) =
        super::vcpu_startup::configure_reserved_vcpu_startup(&vcpu, entry_point, |arch_vcpu| {
            <CurrentArch as CpuUpOps>::set_vcpu_on_args(arch_vcpu, vcpu_id, arg);
        })
    {
        return Err(rollback_reserved_startup_after_error(
            &runtime, &vcpu, vcpu_id, error,
        ));
    }

    let vcpu_task = match crate::runtime::vcpus::build_vcpu_task(&vm, vcpu.clone())
        .and_then(crate::runtime::vcpus::PendingVcpuTask::prepare)
    {
        Ok(vcpu_task) => vcpu_task,
        Err(error) => {
            return Err(rollback_reserved_startup_after_error(
                &runtime, &vcpu, vcpu_id, error,
            ));
        }
    };
    let task_ref = vcpu_task.task_ref().clone();
    if let Err(error) = runtime.publish_reserved_vcpu_task(vcpu_id, task_ref.clone()) {
        return Err(rollback_reserved_startup_after_error(
            &runtime, &vcpu, vcpu_id, error,
        ));
    }
    if let Err(activation_error) = vcpu_task.activate() {
        return Err(rollback_published_startup_after_error(
            &runtime,
            &vcpu,
            vcpu_id,
            &task_ref,
            activation_error,
        ));
    }
    Ok(())
}

fn cancel_task_reservation_after_error(
    runtime: &crate::vm::VmRuntimeHandle,
    vcpu_id: usize,
    startup_error: AxVmError,
) -> AxVmError {
    match runtime.rollback_vcpu_task_slot(vcpu_id, None) {
        Ok(()) => startup_error,
        Err(rollback_error) => AxVmError::host(
            "cancel secondary vCPU task reservation",
            format_args!(
                "startup failed: {startup_error}; task reservation rollback failed: \
                 {rollback_error}"
            ),
        ),
    }
}

fn rollback_reserved_startup_after_error(
    runtime: &crate::vm::VmRuntimeHandle,
    vcpu: &crate::runtime::VCpuRef,
    vcpu_id: usize,
    startup_error: AxVmError,
) -> AxVmError {
    if let Err(state_rollback) = vcpu.cancel_startup() {
        return AxVmError::host(
            "roll back reserved secondary vCPU startup",
            format_args!(
                "startup failed: {startup_error}; state rollback failed: {state_rollback}; task \
                 reservation retained"
            ),
        );
    }
    cancel_task_reservation_after_error(runtime, vcpu_id, startup_error)
}

fn rollback_published_startup_after_error(
    runtime: &crate::vm::VmRuntimeHandle,
    vcpu: &crate::runtime::VCpuRef,
    vcpu_id: usize,
    task_ref: &crate::AxTaskRef,
    startup_error: AxVmError,
) -> AxVmError {
    if let Err(state_rollback) = vcpu.cancel_startup() {
        return AxVmError::host(
            "roll back published secondary vCPU startup",
            format_args!(
                "startup failed: {startup_error}; state rollback failed: {state_rollback}; \
                 published task retained"
            ),
        );
    }
    match runtime.rollback_vcpu_task_slot(vcpu_id, Some(task_ref)) {
        Ok(()) => startup_error,
        Err(task_rollback) => AxVmError::host(
            "roll back published secondary vCPU startup",
            format_args!("startup failed: {startup_error}; task rollback failed: {task_rollback}"),
        ),
    }
}
