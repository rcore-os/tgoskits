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

#[cfg(target_arch = "loongarch64")]
pub mod loongarch_irq;
pub(crate) mod vcpus;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86_irq;

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err, ax_err_type};
#[cfg(target_arch = "x86_64")]
use axvm_types::InterruptTriggerMode;

use crate::{StopReason, VmStatus};

/// The instantiated VM ref type (by `Arc`).
pub type VMRef = crate::AxVMRef;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = crate::vm::AxVCpuRef;

static VMM: crate::WaitQueue = crate::WaitQueue::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize runtime state for already registered VMs.
pub fn init() {
    info!("Initializing VMM...");
}

/// Start the VMM.
pub fn start() {
    info!("VMM starting, booting VMs...");
    for vm in crate::get_vm_list() {
        match vm.start() {
            Ok(_) => {
                RUNNING_VM_COUNT.fetch_add(1, Ordering::Release);
                vcpus::notify_primary_vcpu(vm.id());
                info!("VM[{}] boot success", vm.id())
            }
            Err(err) => warn!("VM[{}] boot failed, error {:?}", vm.id(), err),
        }
    }

    // Do not exit until all VMs are stopped.
    VMM.wait_until(|| {
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

pub fn start_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    let status = vm.status();
    if !matches!(status, VmStatus::Ready | VmStatus::Stopped) {
        return ax_err!(BadState, "VM cannot be started from its current state");
    }

    vm.start()?;
    add_running_vm_count(1);
    vcpus::notify_primary_vcpu(vm_id);
    Ok(())
}

pub fn stop_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    vm.stop(StopReason::Forced)?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn resume_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    vm.resume()?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn reset_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
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

/// Register a native host IRQ as the source for one x86 guest IOAPIC GSI.
#[cfg(target_arch = "x86_64")]
pub(crate) fn register_x86_ioapic_irq_forwarding_route(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
) {
    x86_irq::register_ioapic_irq_forwarding_route(guest_gsi, host_irq);
}

/// Register a native host IRQ and trigger mode as the source for one x86 guest
/// IOAPIC GSI.
#[cfg(target_arch = "x86_64")]
pub(crate) fn register_x86_ioapic_irq_forwarding_route_with_trigger(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: InterruptTriggerMode,
) {
    x86_irq::register_ioapic_irq_forwarding_route_with_trigger(guest_gsi, host_irq, trigger);
}

/// Register a callback to activate one x86 guest IOAPIC GSI after the guest has
/// programmed a usable virtual IOAPIC route for it.
#[cfg(target_arch = "x86_64")]
pub(crate) fn register_x86_ioapic_irq_forwarding_activator(guest_gsi: usize, activator: fn()) {
    x86_irq::register_ioapic_irq_forwarding_activator(guest_gsi, activator);
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
}
