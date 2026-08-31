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

use std::{cell::Cell, format, sync::Arc};

use crate::{
    AsVCpuTask, AxVmResult, GuestPhysAddr, StopReason, VCpuTask, VmStatus, VmVcpuState,
    arch::current::CurrentArch,
    architecture::{ArchOps, Architecture, VcpuRunAction},
    ax_err_type,
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
    vm::{PendingInterrupt, VmRuntimeHandle},
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

#[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
// Five percent of the 1 ms periodic control workload used by the AxVisor
// A/B regression. This bounds one contiguous busy-wait without presenting
// the static profile as an adaptive scheduler policy.
const IDLE_POLL_BUDGET_NS: u64 = 50_000;

/// Per-vCPU runtime policy for the opt-in idle polling profile.
///
/// A polling interval is deliberately bounded: once its deadline expires the
/// ordinary shared wait path resumes, so an idle vCPU cannot remain runnable
/// indefinitely on a cooperative host scheduler.
#[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
#[derive(Default)]
struct IdlePollPolicy {
    deadline_ns: Option<u64>,
    poll_bypass_count: u64,
    poll_fallback_count: u64,
    reported_runtime_observation: bool,
}

/// Result of one ordinary-idle wait decision in the bounded polling profile.
#[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdlePollWaitDecision {
    /// Keep the vCPU runnable inside its bounded polling interval.
    BypassSharedWait,
    /// Return to the ordinary shared wait queue for the stated reason.
    SharedWait(IdlePollFallback),
}

/// Why an ordinary-idle vCPU left its bounded polling interval.
#[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdlePollFallback {
    /// The guest requested a blocking lifecycle wait rather than ordinary idle.
    GuestRequestedBlock,
    /// The local host scheduler has requested preemption.
    PreemptionPending,
    /// The bounded polling interval elapsed.
    BudgetExpired,
}

#[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
impl IdlePollPolicy {
    /// Runs the shared event wait only when the current runtime policy requires it.
    fn wait_for_event_if_required(
        &mut self,
        event_wait: crate::architecture::VcpuEventWait,
        now_ns: u64,
        preemption_pending: bool,
        wait_for_event: impl FnOnce(),
    ) -> IdlePollWaitDecision {
        let decision = self.decide_wait(event_wait, now_ns, preemption_pending);
        if matches!(decision, IdlePollWaitDecision::SharedWait(_)) {
            wait_for_event();
        }
        self.record_decision(decision);
        decision
    }

    /// Selects the host wait action for one vCPU exit.
    fn decide_wait(
        &mut self,
        event_wait: crate::architecture::VcpuEventWait,
        now_ns: u64,
        preemption_pending: bool,
    ) -> IdlePollWaitDecision {
        use crate::architecture::VcpuEventWait;

        match event_wait {
            VcpuEventWait::None => {
                self.deadline_ns = None;
                IdlePollWaitDecision::BypassSharedWait
            }
            VcpuEventWait::Block => {
                self.deadline_ns = None;
                IdlePollWaitDecision::SharedWait(IdlePollFallback::GuestRequestedBlock)
            }
            VcpuEventWait::Poll if preemption_pending => {
                self.deadline_ns = None;
                IdlePollWaitDecision::SharedWait(IdlePollFallback::PreemptionPending)
            }
            VcpuEventWait::Poll if self.poll_budget_expired(now_ns) => {
                IdlePollWaitDecision::SharedWait(IdlePollFallback::BudgetExpired)
            }
            VcpuEventWait::Poll => IdlePollWaitDecision::BypassSharedWait,
        }
    }

    /// Records observable ordinary-idle profile decisions.
    fn record_decision(&mut self, decision: IdlePollWaitDecision) {
        match decision {
            IdlePollWaitDecision::BypassSharedWait if self.deadline_ns.is_some() => {
                self.poll_bypass_count = self.poll_bypass_count.saturating_add(1);
            }
            IdlePollWaitDecision::SharedWait(
                IdlePollFallback::PreemptionPending | IdlePollFallback::BudgetExpired,
            ) => {
                self.poll_fallback_count = self.poll_fallback_count.saturating_add(1);
            }
            IdlePollWaitDecision::BypassSharedWait
            | IdlePollWaitDecision::SharedWait(IdlePollFallback::GuestRequestedBlock) => {}
        }

        if !self.reported_runtime_observation
            && self.poll_bypass_count != 0
            && self.poll_fallback_count != 0
        {
            info!(
                "AXVISOR_RT_POLL_IDLE_RUNTIME_PASSED poll_bypass_count={} poll_fallback_count={}",
                self.poll_bypass_count, self.poll_fallback_count
            );
            self.reported_runtime_observation = true;
        }
    }

    /// Returns whether a bounded polling interval has expired.
    fn poll_budget_expired(&mut self, now_ns: u64) -> bool {
        let deadline_ns = self
            .deadline_ns
            .get_or_insert_with(|| now_ns.saturating_add(IDLE_POLL_BUDGET_NS));
        if now_ns >= *deadline_ns {
            self.deadline_ns = None;
            true
        } else {
            false
        }
    }
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_vcpus: &VmRuntimeHandle, condition: F)
where
    F: Fn() -> bool,
{
    vm_vcpus.wait_until(condition);
}

fn vcpu_start_is_ready(vm_running: bool, task_registered: bool) -> bool {
    vm_running && task_registered
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
    match vm.runtime_handle() {
        Ok(runtime) => runtime.notify_one(),
        Err(err) => warn!("VM[{vm_id}] vCPU runtime not found: {err:?}"),
    }
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id)
        && let Ok(runtime) = vm.runtime_handle()
    {
        runtime.notify_all();
    }
}

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxVmResult {
    queue_pending_interrupt(vm_id, vcpu_id, PendingInterrupt::Normal(vector))
}

pub(crate) fn queue_pending_interrupt(
    vm_id: usize,
    vcpu_id: usize,
    interrupt: PendingInterrupt,
) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let runtime = vm.runtime_handle()?;
    let cpu_id = runtime.queue_pending_interrupt(vcpu_id, interrupt)?;
    runtime.notify_all();
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

/// Notify every shared waiter, then kick the target vCPU's host CPU.
///
/// The vCPU ID selects the IPI destination only. The runtime wait queue is
/// still VM-wide, so this function deliberately retains broadcast wake
/// semantics until AxVM gains per-vCPU wait queues.
pub(crate) fn notify_waiters_and_kick_vcpu(vm_id: usize, vcpu_id: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let runtime = vm.runtime_handle()?;
    let cpu_id = runtime.vcpu_cpu_id(vcpu_id)?;
    runtime.notify_all();
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

pub(crate) fn inject_pending_interrupts<A: Architecture>(
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) {
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };
    let Ok(runtime) = vm.runtime_handle() else {
        warn!("VM[{vm_id}] vCPU runtime not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };
    let interrupts = runtime.drain_pending_interrupts(vcpu_id);

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
        && let Err(err) = vm
            .runtime_handle()
            .and_then(|runtime| runtime.join_all_vcpu_tasks(vm_id))
    {
        warn!("VM[{vm_id}] vCPU runtime cleanup skipped: {err:?}");
    }
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm: &VMRef) {
    if let Ok(runtime) = vm.runtime_handle() {
        runtime.mark_vcpu_running();
    }
}

type CpuOnStartAckLock<T> = std::sync::Mutex<T>;

#[allow(dead_code)]
pub(crate) struct CpuOnStartAck {
    inner: CpuOnStartAckLock<CpuOnStartAckInner>,
}

struct CpuOnStartAckInner {
    started: bool,
    cancelled: bool,
    result: Option<crate::AxVmResult>,
}

#[allow(dead_code)]
impl CpuOnStartAck {
    pub(crate) fn new() -> Self {
        Self {
            inner: CpuOnStartAckLock::new(CpuOnStartAckInner {
                started: false,
                cancelled: false,
                result: None,
            }),
        }
    }

    pub(crate) fn begin_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.cancelled {
            false
        } else {
            inner.started = true;
            true
        }
    }

    pub(crate) fn cancel_before_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.started || inner.result.is_some() {
            false
        } else {
            inner.cancelled = true;
            true
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock_inner().cancelled
    }

    pub(crate) fn complete(&self, result: crate::AxVmResult) {
        self.lock_inner().result = Some(result);
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.lock_inner().result.is_some()
    }

    pub(crate) fn take_result(&self) -> Option<crate::AxVmResult> {
        self.lock_inner().result.take()
    }

    fn lock_inner(&self) -> impl std::ops::DerefMut<Target = CpuOnStartAckInner> + '_ {
        use crate::sync::MutexExt;
        self.inner.lock_unpoisoned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum VcpuOnError {
    AlreadyOn,
    OnPending,
    StartFailed,
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
#[allow(dead_code)]
pub(crate) fn vcpu_on(
    vm: VMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> Result<(), VcpuOnError> {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or(VcpuOnError::StartFailed)?;

    match vcpu.state() {
        VmVcpuState::Free => {}
        VmVcpuState::Starting => return Err(VcpuOnError::OnPending),
        VmVcpuState::Ready | VmVcpuState::Running => return Err(VcpuOnError::AlreadyOn),
        _ => return Err(VcpuOnError::StartFailed),
    }

    vcpu.reserve_for_cpu_on()
        .map_err(|_| VcpuOnError::OnPending)?;

    let start_result = (|| {
        let runtime = vm.runtime_handle().map_err(|_| VcpuOnError::StartFailed)?;
        if runtime.has_vcpu_task(vcpu_id) {
            return Err(VcpuOnError::StartFailed);
        }

        vcpu.set_entry(entry_point)
            .map_err(|_| VcpuOnError::StartFailed)?;
        CurrentArch::set_vcpu_on_args(&vcpu, vcpu_id, arg);

        let ack = Arc::new(CpuOnStartAck::new());
        runtime
            .insert_cpu_on_start_ack(vcpu_id, ack.clone())
            .map_err(|_| VcpuOnError::StartFailed)?;

        let vcpu_task = build_vcpu_task(&vm, vcpu.clone());
        spawn_registered_vcpu_task(vm.id(), vcpu_id, runtime.clone(), vcpu_task);
        runtime.notify_all();

        runtime.wait_until(|| ack.is_complete() || !vm.running());

        if !ack.is_complete() && !vm.running() {
            if ack.cancel_before_startup() {
                runtime.notify_all();

                if let Some(task) = runtime.remove_vcpu_task(vcpu_id) {
                    let _ = task.join();
                }

                runtime.remove_cpu_on_start_ack(vcpu_id);
                return Err(VcpuOnError::StartFailed);
            }

            runtime.wait_until(|| ack.is_complete());
        }

        let result = ack.take_result().unwrap_or_else(|| {
            Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup did not complete")
            ))
        });
        runtime.remove_cpu_on_start_ack(vcpu_id);

        if result.is_err() {
            runtime.remove_vcpu_task(vcpu_id);
            return Err(VcpuOnError::StartFailed);
        }

        Ok(())
    })();

    if start_result.is_err() && vcpu.state() == VmVcpuState::Starting {
        vcpu.rollback_cpu_on();
    }
    start_result
}
pub(crate) fn spawn_registered_vcpu_task(
    vm_id: usize,
    vcpu_id: usize,
    runtime: std::sync::Arc<VmRuntimeHandle>,
    task: crate::TaskInner,
) -> crate::AxTaskRef {
    crate::host::task::spawn_task_with(task, |task_ref| {
        runtime
            .add_vcpu_task(vcpu_id, task_ref.clone())
            .unwrap_or_else(|error| {
                panic!("VM[{vm_id}] vCPU[{vcpu_id}] task registration failed: {error}")
            });
    })
}

fn spawn_deferred_reset_task(vm_id: usize) {
    let reset_task = crate::TaskInner::new(
        move || {
            if let Err(err) = crate::runtime::reset_vm(vm_id) {
                warn!("VM[{vm_id}] deferred reset failed: {err:?}");
                crate::host::task::wait_queue_wake(&super::VMM, 1);
            }
        },
        format!("VM[{vm_id}]-reset"),
        KERNEL_STACK_SIZE,
    );
    crate::host::task::spawn_task(reset_task);
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
    let Ok(runtime) = vm.runtime_handle() else {
        warn!("VM[{vm_id}] vCPU runtime not found, VCpu[{vcpu_id}] exiting");
        return;
    };

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    let cpu_on_start_ack = runtime.cpu_on_start_ack(vcpu_id);
    wait_for(&runtime, || {
        vcpu_start_is_ready(vm.running(), runtime.has_vcpu_task(vcpu_id))
            || cpu_on_start_ack
                .as_ref()
                .is_some_and(|ack| ack.is_cancelled())
    });

    if let Some(ack) = &cpu_on_start_ack {
        if !ack.begin_startup() {
            ack.complete(Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup was cancelled")
            )));
            runtime.notify_all();
            return;
        }

        match vcpu.bind_after_cpu_on_or_rollback() {
            Ok(()) => {
                CurrentArch::before_first_run(&vm, &vcpu);
                runtime.publish_cpu_on_start_success(ack);
                runtime.notify_all();
            }
            Err(err) => {
                ack.complete(Err(err));
                runtime.notify_all();
                runtime.remove_cpu_on_start_ack(vcpu_id);
                runtime.remove_vcpu_task(vcpu_id);
                return;
            }
        }
    } else {
        CurrentArch::before_first_run(&vm, &vcpu);
        mark_vcpu_running(&vm);
    }

    info!(
        "VM[{}] VCpu[{}] running on CPU{}...",
        vm.id(),
        vcpu.id(),
        crate::host::cpu::current_id()
    );
    // Independent re-execution evidence is published *after* each guest entry,
    // at the bottom of the run loop (after `run_vcpu` returns). Every wake from
    // suspend below also re-enters the guest and is counted there, so the
    // control plane can prove the guest actually re-executed after resume/reset.

    #[cfg(all(feature = "rt-poll-idle", target_arch = "aarch64"))]
    let mut idle_poll_policy = IdlePollPolicy::default();

    loop {
        if vcpu_id == 0 {
            // Host services only publish a request and wake this task. Polling
            // here avoids running virtual-device and VGIC callbacks in host
            // console context, where an idle guest may otherwise stall input.
            let _ = poll_primary_vcpu_devices_with(&runtime, || poll_vm_devices(&vm));
        }

        // The guest has entered (and exited) for this run-loop iteration: the
        // control plane reads this as independent re-execution evidence. It is
        // published *only* after a successful `run_vcpu`, so a failed entry
        // (bind / `before_vcpu_run` / `vcpu.run()` / exit handling that returns
        // `Err` before the guest ever runs) cannot advance the counter — a
        // broken wake path that only flips the status without ever re-entering
        // the guest cannot advance it either.
        let action = match CurrentArch::run_vcpu(&vm, &vcpu) {
            Ok(action) => Some(action),
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                if let Err(err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                    warn!("VM[{vm_id}] shutdown failed after vCPU error: {err:?}");
                }
                // Notify all vCPUs to wake up to check the shutdown flag. The
                // guest never entered on this iteration, so skip the
                // re-execution evidence below; the suspend/stopping checks that
                // follow still run and break the loop.
                notify_all_vcpus(vm_id);
                None
            }
        };

        if let Some(action) = action {
            runtime.inc_guest_entry();

            match action {
                VcpuRunAction {
                    exits_vcpu: true, ..
                } => {
                    if let Err(err) = vcpu.power_off_after_cpu_off() {
                        warn!("VM[{vm_id}] VCpu[{vcpu_id}] CPU_OFF cleanup failed: {err:?}");
                    }
                    runtime.remove_vcpu_task(vcpu_id);
                    if !runtime.consume_cpu_off_reservation(vcpu_id) {
                        let _ = runtime.mark_vcpu_exiting();
                    }
                    break;
                }
                VcpuRunAction {
                    resets_vm: true, ..
                } => {
                    if runtime.request_deferred_reset()
                        && let Err(err) = vm.stop(StopReason::Forced)
                    {
                        if vm.stopping() {
                            warn!(
                                "VM[{vm_id}] reset requested while VM is already stopping: {err:?}"
                            );
                        } else {
                            let _ = runtime.take_deferred_reset_request();
                            warn!("VM[{vm_id}] failed to request deferred reset stop: {err:?}");
                            if let Err(stop_err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                                warn!(
                                    "VM[{vm_id}] shutdown after reset request failure failed: \
                                     {stop_err:?}"
                                );
                            }
                        }
                    }
                    notify_all_vcpus(vm_id);
                }
                VcpuRunAction {
                    stop_reason: Some(reason),
                    ..
                } => {
                    if let Err(err) = vm.stop(reason) {
                        warn!("VM[{vm_id}] shutdown failed: {err:?}");
                    }
                    notify_all_vcpus(vm_id);
                }
                VcpuRunAction { event_wait, .. } => {
                    #[cfg(all(feature = "rt-poll-idle", target_arch = "aarch64"))]
                    idle_poll_policy.wait_for_event_if_required(
                        event_wait,
                        ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos(),
                        crate::host::task::preemption_pending(),
                        || CurrentArch::wait_for_vcpu_event(&vm, &vcpu, &runtime),
                    );
                    #[cfg(not(all(feature = "rt-poll-idle", target_arch = "aarch64")))]
                    let requires_shared_wait = event_wait.uses_shared_wait();

                    #[cfg(not(all(feature = "rt-poll-idle", target_arch = "aarch64")))]
                    if requires_shared_wait {
                        CurrentArch::wait_for_vcpu_event(&vm, &vcpu, &runtime);
                    }
                }
            }
        }

        // Check if the VM is suspended
        if vm.suspending() {
            debug!(
                "VM[{}] VCpu[{}] is suspended, waiting for resume...",
                vm_id, vcpu_id
            );
            // Park the vCPU until it is resumed. The wait condition closure is
            // evaluated by the wait queue while holding its lock, immediately
            // before the task is enqueued, so publishing the pause-completion
            // evidence inside it makes the signal visible only once the vCPU is
            // genuinely committed to blocking. A resume that races in before the
            // vCPU reaches the wait keeps the suspend flag clear and makes the
            // condition already true, so the vCPU never publishes a park and
            // never blocks; the control-plane probe then times out waiting for
            // `guest_park_count`, correctly reporting that the pause did not
            // genuinely complete instead of passing on a fake.
            let parked = Cell::new(false);
            wait_for(&runtime, || {
                if !vm.suspending() {
                    return true;
                }
                if !parked.get() {
                    runtime.inc_guest_park();
                    parked.set(true);
                }
                false
            });
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
                let reset_after_stop = runtime.take_deferred_reset_request();
                info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");

                if let Err(err) = CurrentArch::on_last_vcpu_exit(&vm) {
                    warn!("VM[{vm_id}] architecture device cleanup failed: {err:?}");
                    runtime.record_lifecycle_error(err);
                }
                if let Err(err) = vm.finish_stop() {
                    warn!("VM[{vm_id}] finish stop failed: {err:?}");
                    runtime.record_lifecycle_error(err);
                } else {
                    info!("VM[{}] state changed to Stopped", vm_id);
                }

                sub_running_vm_count(1);
                if reset_after_stop {
                    spawn_deferred_reset_task(vm_id);
                } else {
                    crate::host::task::wait_queue_wake(&super::VMM, 1);
                }
            }

            break;
        }

        // AxVM may run on ArceOS's cooperative FIFO scheduler. Polling does
        // not enter the shared wait queue, but it still gives local host
        // services a scheduling point after checking the timer wheel.
        crate::host::task::yield_now();
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}

fn poll_primary_vcpu_devices_with(runtime: &VmRuntimeHandle, poll_devices: impl FnOnce()) -> bool {
    let consumed_request = runtime.take_device_poll_request();
    poll_devices();
    consumed_request
}

pub(super) fn poll_vm_devices(vm: &VMRef) {
    poll_vm_input_devices(vm);
    poll_vm_dma_devices(vm);
}

pub(super) fn poll_vm_input_devices(vm: &VMRef) {
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    for device in devices.iter_pollable_dev() {
        if let Err(error) = device.poll(now_ns) {
            warn!("VM[{}] failed to poll virtual device: {error}", vm.id());
        }
    }
}

fn poll_vm_dma_devices(vm: &VMRef) {
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    let mut memory = crate::vm::VmGuestMemoryAccess::new(vm);
    devices.poll_dma_devices(now_ns, &mut memory, |result| {
        if let Err(error) = result {
            warn!("VM[{}] failed to poll DMA virtual device: {error}", vm.id());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powered_down_vcpu_keeps_shared_wait_in_polling_profile() {
        assert!(crate::architecture::VcpuEventWait::Block.uses_shared_wait());
    }

    #[test]
    fn ordinary_idle_wait_uses_profile_selected_path() {
        assert_eq!(
            crate::architecture::VcpuEventWait::Poll.uses_shared_wait(),
            !cfg!(feature = "rt-poll-idle")
        );
    }

    #[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
    #[test]
    fn idle_poll_policy_only_bypasses_shared_wait_within_the_poll_budget() {
        use crate::architecture::VcpuEventWait;

        let mut policy = IdlePollPolicy::default();
        let shared_wait_count = std::cell::Cell::new(0);

        let first_decision =
            policy.wait_for_event_if_required(VcpuEventWait::Poll, 10, false, || {
                shared_wait_count.set(shared_wait_count.get() + 1);
            });
        let second_decision =
            policy.wait_for_event_if_required(VcpuEventWait::Poll, 10, false, || {
                shared_wait_count.set(shared_wait_count.get() + 1);
            });
        assert_eq!(first_decision, IdlePollWaitDecision::BypassSharedWait);
        assert_eq!(second_decision, IdlePollWaitDecision::BypassSharedWait);
        assert_eq!(shared_wait_count.get(), 0);

        let budget_decision = policy.wait_for_event_if_required(
            VcpuEventWait::Poll,
            10 + IDLE_POLL_BUDGET_NS,
            false,
            || {
                shared_wait_count.set(shared_wait_count.get() + 1);
            },
        );
        assert_eq!(
            budget_decision,
            IdlePollWaitDecision::SharedWait(IdlePollFallback::BudgetExpired)
        );
        assert_eq!(policy.poll_bypass_count, 2);
        assert_eq!(policy.poll_fallback_count, 1);
        policy.wait_for_event_if_required(VcpuEventWait::Block, 20, false, || {
            shared_wait_count.set(shared_wait_count.get() + 1);
        });
        policy.wait_for_event_if_required(VcpuEventWait::None, 20, false, || {
            shared_wait_count.set(shared_wait_count.get() + 1);
        });
        assert_eq!(shared_wait_count.get(), 2);
    }

    #[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
    #[test]
    fn idle_poll_policy_retreats_when_the_scheduler_requests_preemption() {
        use crate::architecture::VcpuEventWait;

        let mut policy = IdlePollPolicy::default();
        let shared_wait_count = std::cell::Cell::new(0);

        policy.wait_for_event_if_required(VcpuEventWait::Poll, 10, false, || {});
        policy.wait_for_event_if_required(VcpuEventWait::Poll, 11, true, || {
            shared_wait_count.set(shared_wait_count.get() + 1);
        });
        assert_eq!(shared_wait_count.get(), 1);

        policy.wait_for_event_if_required(VcpuEventWait::Poll, 12, false, || {
            shared_wait_count.set(shared_wait_count.get() + 1);
        });
        assert_eq!(shared_wait_count.get(), 1);
    }

    #[cfg(all(feature = "rt-poll-idle", any(target_arch = "aarch64", test)))]
    #[test]
    fn idle_poll_policy_never_waits_for_non_idle_exits_when_preemption_is_pending() {
        use crate::architecture::VcpuEventWait;

        let mut policy = IdlePollPolicy::default();
        let shared_wait_count = std::cell::Cell::new(0);

        policy.wait_for_event_if_required(VcpuEventWait::None, 10, true, || {
            shared_wait_count.set(shared_wait_count.get() + 1);
        });

        assert_eq!(shared_wait_count.get(), 0);
    }

    #[test]
    fn vcpu_waits_for_runtime_registration_before_entering_guest() {
        assert!(!vcpu_start_is_ready(true, false));
        assert!(vcpu_start_is_ready(true, true));
        assert!(!vcpu_start_is_ready(false, true));
    }

    #[test]
    fn request_published_before_wfi_snapshot_prevents_sleep_and_is_consumed_once() {
        let runtime = Arc::new(VmRuntimeHandle::new());
        let request_published = Arc::new(std::sync::Barrier::new(2));
        let notifier_runtime = runtime.clone();
        let notifier_published = request_published.clone();
        let notifier = std::thread::spawn(move || {
            notifier_runtime.notify_device_poll();
            notifier_published.wait();
        });

        request_published.wait();
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        let wait_count = std::cell::Cell::new(0);
        crate::vm::wait_for_vcpu_event_if_idle(
            &runtime,
            &wait_snapshot,
            || true,
            |_| wait_count.set(wait_count.get() + 1),
        );

        assert_eq!(wait_count.get(), 0);
        let poll_count = std::cell::Cell::new(0);
        let consumed = poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        });

        assert!(consumed);
        assert_eq!(poll_count.get(), 1);
        assert!(!poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        }));
        assert_eq!(poll_count.get(), 2);
        notifier.join().unwrap();
    }

    #[test]
    fn request_published_at_wait_boundary_prevents_sleep_and_is_consumed_once() {
        let runtime = Arc::new(VmRuntimeHandle::new());
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        let wait_boundary_reached = Arc::new(std::sync::Barrier::new(2));
        let request_published = Arc::new(std::sync::Barrier::new(2));
        let notifier_runtime = runtime.clone();
        let notifier_wait_boundary = wait_boundary_reached.clone();
        let notifier_published = request_published.clone();
        let notifier = std::thread::spawn(move || {
            notifier_wait_boundary.wait();
            notifier_runtime.notify_device_poll();
            notifier_published.wait();
        });

        let sleep_count = std::cell::Cell::new(0);
        crate::vm::wait_for_vcpu_event_if_idle(
            &runtime,
            &wait_snapshot,
            || true,
            |wake_condition| {
                wait_boundary_reached.wait();
                request_published.wait();
                if !wake_condition() {
                    sleep_count.set(sleep_count.get() + 1);
                }
            },
        );

        assert_eq!(sleep_count.get(), 0);
        let poll_count = std::cell::Cell::new(0);
        let consumed = poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        });

        assert!(consumed);
        assert_eq!(poll_count.get(), 1);
        assert!(!poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        }));
        assert_eq!(poll_count.get(), 2);
        notifier.join().unwrap();
    }

    #[test]
    fn cpu_on_start_ack_cancel_before_startup_blocks_late_startup() {
        let ack = CpuOnStartAck::new();

        assert!(ack.cancel_before_startup());
        assert!(ack.is_cancelled());
        assert!(!ack.begin_startup());

        ack.complete(Err(ax_err_type!(
            BadState,
            "vCPU 1 CPU_ON startup was cancelled"
        )));

        assert!(ack.is_complete());
        assert!(ack.take_result().unwrap().is_err());
    }
}
