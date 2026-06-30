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

use ax_errno::{AxResult, ax_err_type};
#[cfg(target_arch = "riscv64")]
use riscv_vcpu::GprIndex as RiscvGprIndex;

use crate::{
    AsVCpuTask, AxVCpuExitReason, CpuMask, GuestPhysAddr, StopReason, VCpuState, VCpuTask,
    VmStatus,
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
    vm::{PendingInterrupt, VmRuntimeHandle},
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

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxResult {
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

#[cfg(target_arch = "loongarch64")]
pub(crate) fn queue_external_interrupt(
    vm_id: usize,
    vcpu_id: usize,
    vector: usize,
    physical_irq: usize,
) -> AxResult {
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

pub(crate) fn inject_pending_interrupts(vm_id: usize, vcpu_id: usize, vcpu: &VCpuRef) {
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
        match interrupt {
            PendingInterrupt::Normal(vector) => {
                trace!("Injecting queued interrupt {vector:#x} into VM[{vm_id}] VCpu[{vcpu_id}]");
                if let Err(err) = vcpu.inject_interrupt(vector) {
                    warn!(
                        "Failed to inject queued interrupt {vector:#x} into VM[{vm_id}] \
                         VCpu[{vcpu_id}]: {err:?}"
                    );
                }
            }
            #[cfg(target_arch = "loongarch64")]
            PendingInterrupt::LoongArchExternal {
                vector,
                physical_irq,
            } => {
                let Some(vm) = crate::get_vm_by_id(vm_id) else {
                    warn!("VM[{vm_id}] not found while injecting queued LoongArch external IRQ");
                    continue;
                };
                let Some(vector) = vm.loongarch_external_irq_vector(vector, physical_irq) else {
                    trace!(
                        "Queued LoongArch external interrupt physical_irq={physical_irq:#x} is \
                         masked in VM[{vm_id}]"
                    );
                    continue;
                };
                trace!(
                    "Injecting queued LoongArch external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x} into VM[{vm_id}] VCpu[{vcpu_id}]"
                );
                if let Err(err) = vcpu
                    .get_arch_vcpu()
                    .inject_external_interrupt(vector, physical_irq)
                {
                    warn!(
                        "Failed to inject queued LoongArch external interrupt vector={vector:#x}, \
                         physical_irq={physical_irq:#x} into VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
                    );
                }
            }
        }
    }
}

fn ipi_targets(
    vm: &VMRef,
    current_vcpu_id: usize,
    target_cpu: u64,
    target_cpu_aux: u64,
    send_to_all: bool,
    send_to_self: bool,
) -> CpuMask<64> {
    let mut targets = CpuMask::new();

    if send_to_all {
        for vcpu in vm.vcpu_list() {
            if vcpu.id() != current_vcpu_id {
                targets.set(vcpu.id(), true);
            }
        }
    } else if send_to_self {
        targets.set(current_vcpu_id, true);
    } else {
        #[cfg(target_arch = "aarch64")]
        {
            for (vcpu_id, _, phys_id) in vm.get_vcpu_affinities_pcpu_ids() {
                let affinity = phys_id as u64;
                let aff0 = affinity & 0xff;
                let aff123 = affinity & !0xff;
                if aff123 == target_cpu && aff0 < 16 && (target_cpu_aux & (1u64 << aff0)) != 0 {
                    targets.set(vcpu_id, true);
                }
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = target_cpu_aux;
            targets.set(target_cpu as usize, true);
        }
    }

    targets
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

/// Boot target VCpu on the specified VM.
/// This function is used to boot a secondary VCpu on a VM, setting the entry point and argument for the VCpu.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM on which the VCpu is to be booted.
/// * `vcpu_id` - The ID of the VCpu to be booted.
/// * `entry_point` - The entry point of the VCpu.
/// * `arg` - The argument to be passed to the VCpu.
fn vcpu_on(vm: VMRef, vcpu_id: usize, entry_point: GuestPhysAddr, arg: usize) -> AxResult {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} not found")))?;
    if vcpu.state() != VCpuState::Free {
        return Err(ax_err_type!(
            BadState,
            format!("vCPU {} invalid state {:?}", vcpu.id(), vcpu.state())
        ));
    }

    vcpu.set_entry(entry_point)?;
    #[cfg(not(target_arch = "riscv64"))]
    vcpu.set_gpr(0, arg);

    #[cfg(target_arch = "riscv64")]
    {
        info!(
            "vcpu_on: vcpu[{}] entry={:x} opaque={:x}",
            vcpu_id, entry_point, arg
        );
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(RiscvGprIndex::A1 as usize, arg);
    }

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

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(crate::AxTaskExt::from_impl(inner));

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
    #[cfg(target_arch = "x86_64")]
    super::x86_irq::enable_ioapic_irq_forwarding(&vm, &vcpu);
    mark_vcpu_running(&vm);

    loop {
        inject_pending_interrupts(vm_id, vcpu_id, &vcpu);

        #[cfg(target_arch = "x86_64")]
        super::x86_irq::drain_pending_ioapic_irqs(&vm, &vcpu);
        #[cfg(target_arch = "x86_64")]
        super::x86_irq::activate_ready_ioapic_forwarding_routes(&vm);

        match vm.run_vcpu(vcpu_id) {
            Ok(exit_reason) => match exit_reason {
                AxVCpuExitReason::Hypercall { nr, args } => {
                    debug!("Hypercall [{nr}] args {args:x?}");
                    use crate::runtime::hvc::HyperCall;

                    match HyperCall::new(vm.clone(), nr, args) {
                        Ok(hypercall) => {
                            let ret_val = match hypercall.execute() {
                                Ok(ret_val) => ret_val as isize,
                                Err(err) => {
                                    warn!("Hypercall [{nr:#x}] failed: {err:?}");
                                    -1
                                }
                            };
                            vcpu.set_return_value(ret_val as usize);
                        }
                        Err(err) => {
                            warn!("Hypercall [{nr:#x}] failed: {err:?}");
                        }
                    }
                }
                AxVCpuExitReason::FailEntry {
                    hardware_entry_failure_reason,
                } => {
                    warn!(
                        "VM[{vm_id}] VCpu[{vcpu_id}] run failed with exit code \
                         {hardware_entry_failure_reason}"
                    );
                }
                AxVCpuExitReason::ExternalInterrupt { vector } => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] get irq {vector}");

                    // TODO: maybe move this irq dispatcher to lower layer to accelerate the interrupt handling
                    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
                    crate::host::arceos::dispatch_host_irq(vector as usize);
                    #[cfg(target_arch = "riscv64")]
                    vcpu.with_current_cpu_set(|| {
                        crate::host::arceos::dispatch_host_irq(vector as usize);
                        vcpu.get_arch_vcpu().latch_hvip_from_hw();
                    });
                    crate::check_timer_events();
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                }
                AxVCpuExitReason::PreemptionTimer => {
                    crate::timer::check_events();
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_due_pit_irq0(&vm, &vcpu);
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                }
                AxVCpuExitReason::InterruptEnd { vector: _vector } => {
                    #[cfg(target_arch = "x86_64")]
                    if let Some(vector) = _vector {
                        super::x86_irq::inject_pending_ioapic_irq_after_eoi(&vm, &vcpu, vector);
                    }
                }
                AxVCpuExitReason::Halt => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] Halt");
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_due_pit_irq0(&vm, &vcpu);
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                    #[cfg(target_arch = "x86_64")]
                    continue;
                    #[cfg(not(target_arch = "x86_64"))]
                    wait(&runtime)
                }
                AxVCpuExitReason::Idle => {
                    trace!("VM[{vm_id}] run VCpu[{vcpu_id}] Idle");
                    #[cfg(target_arch = "loongarch64")]
                    {
                        crate::check_timer_events();
                        if vcpu.get_arch_vcpu().has_enabled_pending_interrupt() {
                            trace!(
                                "VM[{vm_id}] VCpu[{vcpu_id}] skips idle wait because guest has \
                                 enabled pending interrupt"
                            );
                            continue;
                        }
                        let idle_timeout = vcpu.get_arch_vcpu().idle_wait_timeout();
                        trace!("VM[{vm_id}] VCpu[{vcpu_id}] host idle wait for {idle_timeout:?}");
                        ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(true);
                        ax_std::os::arceos::modules::ax_hal::asm::enable_irqs();
                        ax_std::os::arceos::modules::ax_hal::time::busy_wait(idle_timeout);
                        ax_std::os::arceos::modules::ax_hal::asm::disable_irqs();
                        ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(false);
                    }
                    #[cfg(not(target_arch = "loongarch64"))]
                    {
                        crate::check_timer_events();
                    }
                }
                AxVCpuExitReason::Nothing => {}
                AxVCpuExitReason::CpuDown { _state } => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] CpuDown state {_state:#x}");
                    wait(&runtime)
                }
                AxVCpuExitReason::CpuUp {
                    target_cpu,
                    entry_point,
                    arg,
                } => {
                    info!(
                        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{target_cpu}] \
                         entry_point={entry_point:x} arg={arg:#x}"
                    );

                    // Get the mapping relationship between all vCPUs and physical CPUs from the configuration
                    let vcpu_mappings = vm.get_vcpu_affinities_pcpu_ids();

                    // Find the vCPU ID corresponding to the physical ID
                    let Some(target_vcpu_id) =
                        vcpu_mappings.iter().find_map(|(vcpu_id, _, phys_id)| {
                            (*phys_id == target_cpu as usize).then_some(*vcpu_id)
                        })
                    else {
                        warn!("Physical CPU ID {target_cpu} not found in VM configuration");
                        vcpu.set_return_value(usize::MAX);
                        continue;
                    };

                    match vcpu_on(vm.clone(), target_vcpu_id, entry_point, arg as _) {
                        Ok(()) => {
                            #[cfg(not(target_arch = "riscv64"))]
                            vcpu.set_gpr(0, 0);
                            #[cfg(target_arch = "riscv64")]
                            vcpu.set_gpr(RiscvGprIndex::A0 as usize, 0);
                        }
                        Err(err) => {
                            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
                            vcpu.set_return_value(usize::MAX);
                        }
                    }
                }
                AxVCpuExitReason::SystemDown => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] SystemDown");
                    if let Err(err) = vm.stop(StopReason::SystemDown) {
                        warn!("VM[{vm_id}] shutdown failed: {err:?}");
                    }
                    // Notify all vCPUs to wake up to check the shutdown flag
                    notify_all_vcpus(vm_id);
                }
                AxVCpuExitReason::SendIPI {
                    target_cpu,
                    target_cpu_aux,
                    send_to_all,
                    send_to_self,
                    vector,
                } => {
                    debug!(
                        "VM[{vm_id}] run VCpu[{vcpu_id}] SendIPI, target_cpu={target_cpu:#x}, \
                         target_cpu_aux={target_cpu_aux:#x}, vector={vector}",
                    );
                    let targets = ipi_targets(
                        &vm,
                        vcpu_id,
                        target_cpu,
                        target_cpu_aux,
                        send_to_all,
                        send_to_self,
                    );
                    if targets.is_empty() {
                        warn!(
                            "VM[{vm_id}] SendIPI has no target: target_cpu={target_cpu:#x}, \
                             target_cpu_aux={target_cpu_aux:#x}"
                        );
                        continue;
                    }

                    if targets.get(vcpu_id) {
                        crate::inject_current_vcpu_interrupt(vector as _)
                            .expect("failed to inject self IPI into current vCPU");
                    }
                    let mut remote_targets = targets;
                    remote_targets.set(vcpu_id, false);
                    if !remote_targets.is_empty()
                        && let Err(err) = vm.inject_interrupt_to_vcpu(remote_targets, vector as _)
                    {
                        warn!(
                            "Failed to inject interrupt {vector} to VM[{vm_id}] targets \
                             {remote_targets:?}: {err:?}"
                        );
                    }
                }
                e => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] unhandled vmexit: {e:?}");
                }
            },
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

                #[cfg(target_arch = "x86_64")]
                super::x86_irq::disable_ioapic_irq_forwarding_for_vm(vm_id);

                sub_running_vm_count(1);
                crate::host::task::wait_queue_wake(&super::VMM, 1);
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}
