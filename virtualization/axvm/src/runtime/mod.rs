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
mod ivc;
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
pub fn notify_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    notify_vm_with_device_poll(
        || vcpus::poll_vm_devices(&vm),
        || vcpus::notify_primary_vcpu(vm_id),
    );
    Ok(())
}

fn notify_vm_with_device_poll(poll_devices: impl FnOnce(), wake_vcpu: impl FnOnce()) {
    poll_devices();
    wake_vcpu();
}

pub fn stop_vm(vm_id: usize) -> AxVmResult {
    let vm = vm_by_id(vm_id)?;
    vm.stop(StopReason::Forced)?;
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
    use std::{cell::RefCell, vec::Vec};

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
    fn console_notification_polls_devices_before_waking_vcpu() {
        let steps = RefCell::new(Vec::new());
        notify_vm_with_device_poll(
            || steps.borrow_mut().push("poll"),
            || {
                assert_eq!(steps.borrow().as_slice(), ["poll"]);
                steps.borrow_mut().push("wake");
            },
        );
        assert_eq!(steps.into_inner(), ["poll", "wake"]);
    }
}
