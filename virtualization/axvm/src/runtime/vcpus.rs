// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::format;

use axvmconfig::VcpuTaskAffinity;

use crate::{
    AsVCpuTask, AxVmError, AxVmResult, StopReason, VCpuTask, VmStatus,
    arch::{ArchOps, CurrentArch, VcpuRunAction},
    ax_err_type,
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
    vm::VmRuntimeHandle,
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

#[must_use = "a pending vCPU task must be prepared and published before activation"]
pub(crate) struct PendingVcpuTask {
    task: crate::TaskInner,
    initial_cpu: usize,
}

#[must_use = "a prepared vCPU task remains non-runnable until it is activated"]
pub(crate) struct PreparedVcpuTask {
    task: crate::host::task::PreparedTask,
}

impl PendingVcpuTask {
    pub(crate) fn prepare(self) -> AxVmResult<PreparedVcpuTask> {
        let task = crate::host::task::prepare_task_with_initial_cpu(self.task, self.initial_cpu)?;
        Ok(PreparedVcpuTask { task })
    }
}

impl PreparedVcpuTask {
    pub(crate) fn task_ref(&self) -> &crate::AxTaskRef {
        self.task.task_ref()
    }

    pub(crate) fn activate(self) -> AxVmResult<crate::AxTaskRef> {
        crate::host::task::activate_task(self.task)
    }
}

/// Blocks a WFI exit until lifecycle state or queued vCPU work changes.
fn wait(vm: &VMRef, vm_vcpus: &VmRuntimeHandle, vcpu_id: usize) {
    vm_vcpus
        .wait_until(|| vm.status() != VmStatus::Running || vm_vcpus.has_pending_vcpu_work(vcpu_id));
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_vcpus: &VmRuntimeHandle, condition: F)
where
    F: Fn() -> bool,
{
    vm_vcpus.wait_until(condition);
}

/// Notifies the primary VCpu task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary VCpu of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus are to be notified.
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary VCpu is the first and **only** VCpu in the list.
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found while notifying primary vCPU");
        return;
    };
    if let Err(err) = vm.with_runtime(|runtime| {
        runtime.notify_one();
        Ok(())
    }) {
        warn!("VM[{vm_id}] vCPU runtime not found: {err:?}");
    }
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id) {
        let _ = vm.with_runtime(|runtime| {
            runtime.notify_all();
            Ok(())
        });
    }
}

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let cpu_id = vm.with_runtime(|runtime| runtime.queue_interrupt(vcpu_id, vector))?;
    vm.with_runtime(|runtime| {
        runtime.notify_all();
        Ok(())
    })?;
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

#[expect(
    dead_code,
    reason = "only the LoongArch IRQ backend queues physical interrupts"
)]
pub(crate) fn queue_external_interrupt(
    vm_id: usize,
    vcpu_id: usize,
    vector: usize,
    physical_irq: usize,
) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let cpu_id =
        vm.with_runtime(|runtime| runtime.queue_external_interrupt(vcpu_id, vector, physical_irq))?;
    vm.with_runtime(|runtime| {
        runtime.notify_all();
        Ok(())
    })?;
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

pub(crate) fn inject_pending_interrupts<A: ArchOps>(
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) {
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };
    let Ok(interrupts) = vm.with_runtime(|runtime| Ok(runtime.drain_pending_interrupts(vcpu_id)))
    else {
        warn!("VM[{vm_id}] vCPU runtime not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };

    for interrupt in interrupts {
        A::inject_pending_interrupt(&vm, vcpu, interrupt);
    }
}

/// Cleans up VCpu resources for a VM that is being deleted.
/// This removes the VM's entry from the global VCpu wait queue.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu resources should be cleaned up.
///
/// # Note
///
/// This should be called after all VCpu threads have exited to avoid resource leaks.
/// It will join all VCpu tasks to ensure they are fully cleaned up.
pub(crate) fn cleanup_vm_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id)
        && let Err(err) = vm.with_runtime(|runtime| {
            runtime.join_all_vcpu_tasks(vm_id);
            Ok(())
        })
    {
        warn!("VM[{vm_id}] vCPU runtime cleanup skipped: {err:?}");
    }
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm: &VMRef) {
    let _ = vm.with_runtime(|runtime| {
        runtime.mark_vcpu_running();
        Ok(())
    });
}

pub(crate) fn build_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> AxVmResult<PendingVcpuTask> {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = crate::TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    let placement = crate::manager::vcpu_task_placement(vm.id(), vcpu.id()).ok_or_else(|| {
        AxVmError::resource_unavailable(
            "guest CPU partition",
            format_args!(
                "VM[{}] vCPU[{}] has no validated initial placement",
                vm.id(),
                vcpu.id()
            ),
        )
    })?;
    if let VcpuTaskAffinity::CpuMask(requested_mask) = placement.affinity {
        let effective_mask = vcpu_task_cpu_mask(vm.id(), vcpu.id(), requested_mask);
        vcpu_task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(effective_mask));
    }
    if !vcpu_task.cpumask().get(placement.initial_cpu) {
        return Err(AxVmError::invalid_state(
            "prepare vCPU task",
            format_args!(
                "planned initial host CPU {} is outside VM[{}] vCPU[{}] affinity {:?}",
                placement.initial_cpu,
                vm.id(),
                vcpu.id(),
                vcpu_task.cpumask()
            ),
        ));
    }

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(crate::AxTaskExt::from_impl(inner));

    info!(
        "VCpu task {} created {:?}, initial CPU {}",
        vcpu_task.id_name(),
        vcpu_task.cpumask(),
        placement.initial_cpu
    );
    Ok(PendingVcpuTask {
        task: vcpu_task,
        initial_cpu: placement.initial_cpu,
    })
}

fn vcpu_task_cpu_mask(vm_id: usize, vcpu_id: usize, requested_mask: usize) -> usize {
    let enabled_mask = crate::percpu::enabled_cpu_mask();
    if enabled_mask == 0 {
        warn!(
            "VM[{vm_id}] VCpu[{vcpu_id}] has no initialized host CPU mask; using requested mask \
             {requested_mask:#x}"
        );
        return requested_mask;
    }

    let initialized_requested_mask = requested_mask & enabled_mask;
    if initialized_requested_mask != 0 {
        if initialized_requested_mask != requested_mask {
            warn!(
                "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but \
                 only {initialized_requested_mask:#x} is initialized for AxVM"
            );
        }
        return initialized_requested_mask;
    }

    let fallback_mask = enabled_mask.isolate_lowest_one();
    warn!(
        "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but none of \
         those CPUs initialized AxVM; using initialized host CPU mask {fallback_mask:#x}"
    );
    fallback_mask
}

/// The main routine for VCpu task.
/// This function is the entry point for the VCpu tasks, which are spawned for each VCpu of a VM.
///
/// When the VCpu first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the VCpu and handles the various exit reasons.
fn vcpu_run() {
    let curr = crate::host::task::current_task();

    let vm = curr.as_vcpu_task().vm();
    let vcpu = curr.as_vcpu_task().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    let Ok(runtime) = vm.with_runtime(|runtime| Ok(runtime.clone())) else {
        warn!("VM[{vm_id}] vCPU runtime not found, VCpu[{vcpu_id}] exiting");
        return;
    };

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    wait_for(&runtime, || vm.running());

    info!("VM[{}] VCpu[{}] running...", vm.id(), vcpu.id());
    CurrentArch::before_first_run(&vm, &vcpu);
    mark_vcpu_running(&vm);

    loop {
        CurrentArch::before_vcpu_run(&vm, &vcpu);

        match CurrentArch::run_vcpu(&vm, &vcpu) {
            Ok(VcpuRunAction {
                stop_reason: Some(reason),
                ..
            }) => {
                if let Err(err) = vm.stop(reason) {
                    warn!("VM[{vm_id}] shutdown failed: {err:?}");
                }
                notify_all_vcpus(vm_id);
            }
            Ok(VcpuRunAction {
                waits_for_event: true,
                ..
            }) => wait(&vm, &runtime, vcpu_id),
            Ok(VcpuRunAction { .. }) => {}
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                if let Err(err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                    warn!("VM[{vm_id}] shutdown failed after vCPU error: {err:?}");
                }
                // Notify all vCPUs to wake up to check the shutdown flag
                notify_all_vcpus(vm_id);
            }
        }

        // Check if the VM is suspended
        if vm.suspending() {
            debug!(
                "VM[{}] VCpu[{}] is suspended, waiting for resume...",
                vm_id, vcpu_id
            );
            wait_for(&runtime, || !vm.suspending());
            info!("VM[{}] VCpu[{}] resumed from suspend", vm_id, vcpu_id);
            continue;
        }

        // Check if the VM is stopping.
        if vm.stopping() {
            warn!(
                "VM[{}] VCpu[{}] stopping because of VM stopping",
                vm_id, vcpu_id
            );

            if runtime.mark_vcpu_exiting() {
                info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");

                if let Err(err) = vm.finish_stop() {
                    warn!("VM[{vm_id}] finish stop failed: {err:?}");
                }
                info!("VM[{}] state changed to Stopped", vm_id);

                CurrentArch::on_last_vcpu_exit(vm_id);

                sub_running_vm_count(1);
                crate::host::task::wait_queue_wake(&super::VMM, 1);
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}
