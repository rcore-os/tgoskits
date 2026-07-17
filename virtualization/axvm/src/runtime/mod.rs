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

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{AxVmError, AxVmResult, StopReason, VmStatus, ax_err};

/// The instantiated VM ref type (by `Arc`).
pub type VMRef = crate::AxVMRef;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = crate::vm::AxVCpuRef;

static VMM: crate::HostWaitQueueHandle = crate::HostWaitQueueHandle::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

struct RunningVmStartPermit {
    armed: bool,
}

impl RunningVmStartPermit {
    fn reserve() -> Self {
        RUNNING_VM_COUNT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .expect("running VM count overflowed");
        Self { armed: true }
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for RunningVmStartPermit {
    fn drop(&mut self) {
        if self.armed {
            sub_running_vm_count(1);
        }
    }
}

/// One default VM that could not enter its first runtime generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultVmStartFailure {
    vm_id: usize,
    error: AxVmError,
}

impl DefaultVmStartFailure {
    pub(crate) const fn new(vm_id: usize, error: AxVmError) -> Self {
        Self { vm_id, error }
    }

    /// Returns the VM identifier whose start operation failed.
    pub const fn vm_id(&self) -> usize {
        self.vm_id
    }

    /// Returns the typed start failure reported by the VM lifecycle.
    pub const fn error(&self) -> &AxVmError {
        &self.error
    }
}

/// Result of starting every registered default VM and waiting for successes.
///
/// A failed VM is not counted as running. Successfully started peers are still
/// joined before this report is returned, which lets the application revoke
/// guest routes and return any exclusive host resource before surfacing the
/// start failures.
#[derive(Debug, Default)]
#[must_use = "default VM start failures must be reported after resource cleanup"]
pub struct DefaultVmRunReport {
    start_failures: Vec<DefaultVmStartFailure>,
}

impl DefaultVmRunReport {
    /// Returns whether every registered VM entered its runtime generation.
    pub fn all_started(&self) -> bool {
        self.start_failures.is_empty()
    }

    /// Returns every VM start failure in registry traversal order.
    pub fn start_failures(&self) -> &[DefaultVmStartFailure] {
        &self.start_failures
    }

    fn record_start_failure(&mut self, vm_id: usize, error: AxVmError) {
        self.start_failures
            .push(DefaultVmStartFailure::new(vm_id, error));
    }
}

/// Initialize runtime state for already registered VMs.
pub fn init() {
    info!("Initializing VMM...");
}

/// Start the VMM and report VMs that could not enter their runtime generation.
pub fn start() -> DefaultVmRunReport {
    info!("VMM starting, booting VMs...");
    let mut report = DefaultVmRunReport::default();
    let mut started_vms = Vec::new();
    for vm in crate::get_vm_list() {
        let running = RunningVmStartPermit::reserve();
        match vm.start() {
            Ok(_) => {
                running.commit();
                vcpus::notify_primary_vcpu(vm.id());
                info!("VM[{}] boot success", vm.id());
                started_vms.push(vm);
            }
            Err(error) => {
                warn!("VM[{}] boot failed, error {:?}", vm.id(), error);
                report.record_start_failure(vm.id(), error);
            }
        }
    }

    // Do not exit until all VMs are stopped.
    crate::host::task::wait_queue_wait_until(&VMM, || {
        let vm_count = RUNNING_VM_COUNT.load(Ordering::Acquire);
        debug!("a VM exited, current running VM count: {vm_count}");
        vm_count == 0
    });
    for vm in started_vms {
        if let Some(error) = vm.take_startup_failure() {
            report.record_start_failure(vm.id(), error);
        }
    }
    report
        .start_failures
        .sort_by_key(DefaultVmStartFailure::vm_id);
    report
}

pub(crate) fn sub_running_vm_count(count: usize) {
    let previous = RUNNING_VM_COUNT.fetch_sub(count, Ordering::AcqRel);
    assert!(previous >= count, "running VM count underflowed");
    if previous == count {
        crate::host::task::wait_queue_wake(&VMM, 1);
    }
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

    let running = RunningVmStartPermit::reserve();
    vm.start()?;
    running.commit();
    vcpus::notify_primary_vcpu(vm_id);
    Ok(())
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
    let running = reset_starts_counted_runtime(previous_status).then(RunningVmStartPermit::reserve);
    vm.reset()?;
    if let Some(running) = running {
        running.commit();
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
    fn default_vm_run_report_preserves_every_start_failure() {
        let mut report = DefaultVmRunReport::default();
        let first = AxVmError::VmNotFound { vm_id: 7 };
        let second = AxVmError::VmNotFound { vm_id: 11 };

        report.record_start_failure(7, first.clone());
        report.record_start_failure(11, second.clone());

        assert_eq!(
            report.start_failures(),
            [
                DefaultVmStartFailure::new(7, first),
                DefaultVmStartFailure::new(11, second),
            ]
        );
        assert!(!report.all_started());
    }
}
