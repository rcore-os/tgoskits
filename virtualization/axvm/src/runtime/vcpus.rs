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

use crate::{
    AsVCpuTask, AxVmResult, GuestPhysAddr, StopReason, VCpuTask, VmVcpuState,
    arch::{ArchOps, CurrentArch, VcpuRunAction},
    ax_err_type,
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
    vm::VmRuntimeHandle,
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// Blocks the current thread until it is explicitly woken up, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
fn wait(vm_vcpus: &VmRuntimeHandle) {
    vm_vcpus.wait();
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
    publish_pending_interrupt_by_id(vm_id, vcpu_id, crate::vm::PendingInterrupt::Normal(vector))
}

pub(crate) fn publish_pending_interrupt_by_id(
    vm_id: usize,
    vcpu_id: usize,
    interrupt: crate::vm::PendingInterrupt,
) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    publish_pending_interrupt(&vm, vcpu_id, interrupt)
}

/// Durably publishes an interrupt to one existing VM instance.
///
/// A successful return means the runtime inbox owns the interrupt. Waking the
/// vCPU task and sending a scheduler IPI are latency hints after publication;
/// an IPI transport failure must not make callers retry an already committed
/// edge-triggered event.
pub(crate) fn publish_pending_interrupt(
    vm: &VMRef,
    vcpu_id: usize,
    interrupt: crate::vm::PendingInterrupt,
) -> AxVmResult {
    let cpu_id = vm.with_interrupt_runtime(|runtime| {
        let cpu_id = runtime.queue_pending_interrupt(vcpu_id, interrupt)?;
        runtime.notify_all();
        Ok(cpu_id)
    })?;
    if let Err(error) = crate::host::task::send_ipi(cpu_id) {
        warn!(
            "VM[{}] VCpu[{vcpu_id}] interrupt remains published after scheduler IPI failure: \
             {error:?}",
            vm.id()
        );
    }
    Ok(())
}

pub(crate) fn inject_pending_interrupts<A: ArchOps>(
    vm: &VMRef,
    vcpu: &crate::vcpu::BoundVcpu<'_, '_, A::VCpu>,
) -> AxVmResult {
    let interrupts = vm.with_runtime(|runtime| Ok(runtime.drain_pending_interrupts(vcpu.id())))?;

    for interrupt in interrupts {
        A::inject_pending_interrupt(vm, vcpu, interrupt)?;
    }
    Ok(())
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

/// Boot target VCpu on the specified VM.
/// This function is used to boot a secondary VCpu on a VM, setting the entry point and argument for the VCpu.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM on which the VCpu is to be booted.
/// * `vcpu_id` - The ID of the VCpu to be booted.
/// * `entry_point` - The entry point of the VCpu.
/// * `arg` - The argument to be passed to the VCpu.
pub fn vcpu_on(
    vm: crate::AxVMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> AxVmResult {
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

    vcpu.set_entry(entry_point)?;
    CurrentArch::set_vcpu_on_args(&vcpu, vcpu_id, arg);

    let vcpu_task = alloc_vcpu_task(&vm, vcpu);
    vm.with_runtime(|runtime| {
        runtime.add_vcpu_task(vcpu_id, vcpu_task);
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn alloc_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::AxTaskRef {
    crate::host::task::spawn_task(build_vcpu_task(vm, vcpu))
}

pub(crate) fn build_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::TaskInner {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = crate::TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(
            vcpu_task_cpu_mask(vm.id(), vcpu.id(), phys_cpu_set),
        ));
    }

    // The extension keeps only a weak VM reference so its task cannot retain the VM.
    let inner = VCpuTask::new(vm, vcpu);
    vcpu_task.set_vcpu_extension(inner);

    info!(
        "VCpu task {} created {:?}",
        vcpu_task.id_name(),
        vcpu_task.cpumask()
    );
    vcpu_task
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

    let fallback_mask = enabled_mask & enabled_mask.wrapping_neg();
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
    if let Err(error) = CurrentArch::before_first_run(&vm, &vcpu) {
        error!("VM[{vm_id}] VCpu[{vcpu_id}] first-run preparation failed: {error:?}");
        vm.record_startup_failure(error.clone());
        if let Err(stop_error) = vm.stop(StopReason::Fault(format!("{error:?}"))) {
            warn!("VM[{vm_id}] shutdown failed after first-run preparation: {stop_error:?}");
        }
        let no_running_vcpu = runtime.mark_vcpu_startup_failed();
        notify_all_vcpus(vm_id);
        if no_running_vcpu {
            complete_vm_stop(&vm, vm_id, vcpu_id);
        }
        return;
    }
    if !runtime.try_mark_vcpu_running() {
        // A peer failed first-run preparation after this vCPU's hook
        // succeeded. The shared startup-failed bit prevents this task from
        // publishing Running after the VM entered Stopping.
        return;
    }

    loop {
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
            }) => wait(&runtime),
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

            finish_stopping_vcpu(&vm, &runtime, vm_id, vcpu_id);

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}

fn finish_stopping_vcpu(vm: &VMRef, runtime: &VmRuntimeHandle, vm_id: usize, vcpu_id: usize) {
    if !runtime.mark_vcpu_exiting() {
        return;
    }

    complete_vm_stop(vm, vm_id, vcpu_id);
}

fn complete_vm_stop(vm: &VMRef, vm_id: usize, vcpu_id: usize) {
    info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");
    if let Err(err) = vm.finish_stop() {
        warn!("VM[{vm_id}] finish stop failed: {err:?}");
    }
    info!("VM[{vm_id}] state changed to Stopped");
    CurrentArch::on_last_vcpu_exit(vm_id);
    sub_running_vm_count(1);
}
