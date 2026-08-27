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

pub(crate) mod hvc;
pub(crate) mod ivc;
pub(crate) mod vcpus;

mod dispatcher;
mod queue;
use std::sync::atomic::{AtomicUsize, Ordering};

// Re-exported for [`VmRuntimeHandle`](crate::vm::VmRuntimeHandle) which will
// embed the dispatcher as a field and expose it to the vCPU run loop.
#[allow(unused_imports)]
pub(crate) use dispatcher::VcpuIrqDispatcher;

use crate::{AxVmError, AxVmResult, StopReason, VmStatus, ax_err};

/// The instantiated VM ref type (by `Arc`).
pub type VMRef = crate::AxVMRef;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = crate::vm::AxVCpuRef;

static VMM: crate::HostWaitQueueHandle = crate::HostWaitQueueHandle::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize runtime state for already registered VMs.
pub fn init() {
    info!("Initializing VMM...");
}

/// Start the VMM.
pub fn start() {
    launch_all();
    wait_for_all();
}

/// Start all registered VMs and return the IDs that entered Running.
pub fn launch_all() -> std::vec::Vec<usize> {
    info!("VMM starting, booting VMs...");
    let mut started = std::vec::Vec::new();
    for vm in crate::get_vm_list() {
        match vm.start() {
            Ok(_) => {
                RUNNING_VM_COUNT.fetch_add(1, Ordering::Release);
                vcpus::notify_primary_vcpu(vm.id());
                started.push(vm.id());
                info!("VM[{}] boot success", vm.id());
            }
            Err(err) => warn!("VM[{}] boot failed, error {:?}", vm.id(), err),
        }
    }
    started
}

/// Wait until every counted VM runtime has stopped.
pub fn wait_for_all() {
    // Do not exit until all VMs are stopped.
    crate::host::task::wait_queue_wait_until(&VMM, || {
        let vm_count = RUNNING_VM_COUNT.load(Ordering::Acquire);
        debug!("a VM exited, current running VM count: {vm_count}");
        vm_count == 0
    });
}

pub fn add_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_add(count, Ordering::Release);
}

pub fn sub_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_sub(count, Ordering::Release);
}

fn reset_starts_counted_runtime(previous_status: VmStatus) -> bool {
    matches!(
        previous_status,
        VmStatus::Ready
            | VmStatus::Running
            | VmStatus::Paused
            | VmStatus::Stopping
            | VmStatus::Stopped
    )
}

pub fn start_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    let status = vm.status();
    if !matches!(status, VmStatus::Ready | VmStatus::Stopped) {
        return ax_err!(BadState, "VM cannot be started from its current state");
    }

    vm.start()?;
    add_running_vm_count(1);
    vcpus::notify_primary_vcpu(vm_id);
    Ok(())
}

/// Wake the primary vCPU of a VM.
///
/// Single-vCPU guests retain pending device work across the WFI boundary.
/// SMP guests keep the legacy wake-only behavior until AxVM provides a
/// per-vCPU wait queue that can target vCPU0.
pub fn notify_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    let vcpu_num = vm.vcpu_num();
    // `WaitQueue::wait_until` evaluates the vCPU wake predicate while it
    // holds both the wait-queue and run-queue locks. That predicate may read
    // the VM lifecycle state and therefore lock `vm.machine`. Never retain
    // `vm.machine` while notifying the same wait queue, or the notifier and a
    // vCPU entering WFI can deadlock in opposite lock order.
    let runtime = vm.runtime_handle()?;
    notify_runtime_for_device_poll(&runtime, vcpu_num);
    Ok(())
}

fn notify_runtime_for_device_poll(runtime: &crate::vm::VmRuntimeHandle, vcpu_num: usize) {
    if vcpu_num == 1 {
        runtime.notify_device_poll();
    } else {
        // The runtime wait queue is shared by all vCPUs, so notify_one cannot
        // target vCPU0. Keep the legacy wake semantics for SMP guests until a
        // dedicated per-vCPU wake path is available; publishing the shared
        // device-poll flag here could keep a secondary vCPU spinning while
        // the primary vCPU remains asleep.
        runtime.notify_one();
    }
}

pub fn stop_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    if matches!(vm.status(), VmStatus::Running) {
        // `start_vm` flips the status to `Running` synchronously while the
        // vCPU task may still be queued on another CPU. Requesting a stop in
        // that window strands the task in its startup gate (which needs a
        // `Running` window it already missed), so wait for the first vCPU
        // entry before accepting the stop.
        wait_until_vcpu_entered(|| vm.running_vcpu_count() > 0, || vm.stopping())?;
    }
    vm.stop(StopReason::Forced)?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

/// Boundedly wait for at least one vCPU task to enter the guest run loop
/// before a request-stop is accepted.
///
/// `start_vm` flips the VM status to `Running` synchronously while the vCPU
/// task may still be queued on another CPU. If the stop is accepted in that
/// window, the task's startup gate blocks on a `Running` window it already
/// missed and parks forever, so the VM never reaches `Stopped`. Waiting for
/// the first vCPU entry closes that window.
///
/// The wait is bounded (`MAX_YIELDS`, mirroring the `wait_until_stopped`
/// pattern): a vCPU task that never runs yields an error instead of hanging
/// the caller, leaving the VM `Running` so the stop can be retried.
///
/// `pub(crate)` because the destroy/reset quiesce path
/// (`AxVM::stop_and_join_runtime`) applies the same guard: a client may POST
/// `/start` and then immediately DELETE the VM, so `destroy()` must hold the
/// request-stop until the first vCPU entry just like `stop_vm`.
pub(crate) fn wait_until_vcpu_entered(
    vcpu_entered: impl Fn() -> bool,
    vm_stopping: impl Fn() -> bool,
) -> AxVmResult {
    const MAX_YIELDS: usize = 10_000;
    for _ in 0..MAX_YIELDS {
        if vcpu_entered() || vm_stopping() {
            return Ok(());
        }
        crate::host::task::yield_now();
    }
    ax_err!(
        BadState,
        "vCPU task did not enter the guest before request-stop"
    )
}

/// Pause a running VM.
///
/// `vm.pause()` flips the status to `Paused` synchronously; the running vCPUs
/// observe the flag at their next run-loop iteration and park in the
/// suspend-wait (`!suspending()`), so the guest actually suspends
/// asynchronously. The notify wakes any vCPU parked in a WFI/event wait so it
/// can reach that check.
pub fn pause_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    vm.pause()?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn resume_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    vm.resume()?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn reset_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    let previous_status = vm.status();
    vm.reset()?;
    if reset_starts_counted_runtime(previous_status) {
        add_running_vm_count(1);
    }
    vcpus::notify_primary_vcpu(vm_id);
    Ok(())
}

pub fn remove_vm(vm_id: usize) -> Option<VMRef> {
    crate::manager::remove_existing_vm(vm_id)
}

/// Register a prepared VM in the AxVM runtime.
pub fn register_vm(vm: VMRef) -> bool {
    crate::manager::push_existing_vm(vm)
}

fn vm_by_id(vm_id: usize) -> AxVmResult<VMRef> {
    crate::get_vm_by_id(vm_id).ok_or_else(|| missing_vm_error(vm_id))
}

const fn missing_vm_error(vm_id: usize) -> AxVmError {
    AxVmError::VmNotFound { vm_id }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        sync::{Arc, atomic::AtomicBool},
    };

    use super::*;

    #[test]
    fn reset_counts_replacement_runtime_for_every_restartable_state() {
        for status in [
            VmStatus::Ready,
            VmStatus::Running,
            VmStatus::Paused,
            VmStatus::Stopping,
            VmStatus::Stopped,
        ] {
            assert!(
                reset_starts_counted_runtime(status),
                "reset from {status:?} starts a fresh running runtime"
            );
        }
    }

    #[test]
    fn missing_vm_is_reported_with_its_id() {
        let vm_id = usize::MAX;
        assert_eq!(missing_vm_error(vm_id), AxVmError::VmNotFound { vm_id });
    }

    #[test]
    fn smp_notification_does_not_publish_a_shared_device_poll_request() {
        let runtime = crate::vm::VmRuntimeHandle::new();
        let observed_generation = runtime.notification_generation();

        notify_runtime_for_device_poll(&runtime, 2);

        assert!(!runtime.device_poll_requested());
        assert_ne!(runtime.notification_generation(), observed_generation);
    }

    #[test]
    fn single_vcpu_notification_publishes_a_device_poll_request() {
        let runtime = crate::vm::VmRuntimeHandle::new();

        notify_runtime_for_device_poll(&runtime, 1);

        assert!(runtime.device_poll_requested());
    }

    #[test]
    fn request_stop_wait_returns_immediately_once_a_vcpu_has_entered() {
        assert!(wait_until_vcpu_entered(|| true, || false).is_ok());
    }

    #[test]
    fn request_stop_wait_bails_out_when_vm_is_already_stopping() {
        assert!(wait_until_vcpu_entered(|| false, || true).is_ok());
    }

    #[test]
    fn request_stop_wait_times_out_instead_of_accepting_a_never_entering_vcpu() {
        let err = wait_until_vcpu_entered(|| false, || false).unwrap_err();

        assert!(matches!(err, AxVmError::InvalidState { .. }));
    }

    #[test]
    fn request_stop_waits_for_vcpu_entry_when_stop_precedes_entry() {
        // Force the scheduling order that previously stranded the vCPU task:
        // the request-stop arrives while no vCPU task has entered the guest
        // run loop, and the task only enters after the wait has begun. The
        // stop must be held back until entry, never accepted-and-stranded.
        let entered = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let first_poll = Arc::new(std::sync::Barrier::new(2));
        let release_entered = Arc::new(std::sync::Barrier::new(2));

        let entered_for_task = entered.clone();
        let first_poll_for_task = first_poll.clone();
        let release_entered_for_task = release_entered.clone();
        let vcpu_task = std::thread::spawn(move || {
            // The vCPU task is queued but has not entered the guest yet.
            first_poll_for_task.wait();
            release_entered_for_task.wait();
            entered_for_task.store(true, Ordering::Release);
        });

        let entered_for_wait = entered.clone();
        let stopping_for_wait = stopping.clone();
        let poll_count = Cell::new(0);
        let result = wait_until_vcpu_entered(
            || {
                let is_entered = entered_for_wait.load(Ordering::Acquire);
                if poll_count.get() == 0 {
                    // First poll observed the pre-entry state. Only now release
                    // the vCPU task to enter the guest, deterministically
                    // ordering stop-before-entry.
                    poll_count.set(1);
                    first_poll.wait();
                    release_entered.wait();
                }
                is_entered
            },
            || stopping_for_wait.load(Ordering::Acquire),
        );

        vcpu_task.join().unwrap();
        assert!(
            result.is_ok(),
            "stop must wait for vCPU entry, not strand it"
        );
        assert!(
            entered.load(Ordering::Acquire),
            "vCPU task must have entered the guest run loop"
        );
    }
}
