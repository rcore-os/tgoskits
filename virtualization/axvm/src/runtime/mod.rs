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

mod hvc;
mod ivc;

pub(crate) mod vcpus;
#[cfg(target_arch = "x86_64")]
mod x86_irq;

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err, ax_err_type};

/// The instantiated VM ref type (by `Arc`).
pub type VMRef = crate::AxVMRef;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = crate::AxVCpuRef;

static VMM: crate::HostWaitQueueHandle = crate::HostWaitQueueHandle::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize runtime state for already registered VMs.
pub fn init() {
    info!("Initializing VMM...");
    info!("Setting up vcpus...");
    for vm in crate::get_vm_list() {
        vcpus::setup_vm_primary_vcpu(vm);
    }
}

/// Start the VMM.
pub fn start() {
    info!("VMM starting, booting VMs...");
    for vm in crate::get_vm_list() {
        match vm.boot() {
            Ok(_) => {
                RUNNING_VM_COUNT.fetch_add(1, Ordering::Release);
                vcpus::notify_primary_vcpu(vm.id());
                info!("VM[{}] boot success", vm.id())
            }
            Err(err) => warn!("VM[{}] boot failed, error {:?}", vm.id(), err),
        }
    }

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

pub fn start_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    let status = vm.vm_status();
    if !matches!(status, crate::VMStatus::Loaded | crate::VMStatus::Stopped) {
        return ax_err!(BadState, "VM cannot be started from its current state");
    }

    vcpus::setup_vm_primary_vcpu(vm.clone());
    vm.boot()?;
    add_running_vm_count(1);
    vcpus::notify_primary_vcpu(vm_id);
    Ok(())
}

pub fn stop_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    vm.shutdown()?;
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn resume_vm(vm_id: usize) -> AxResult {
    let vm = crate::get_vm_by_id(vm_id).ok_or_else(|| ax_err_type!(NotFound, "VM not found"))?;
    vm.set_vm_status(crate::VMStatus::Running);
    vcpus::notify_all_vcpus(vm_id);
    Ok(())
}

pub fn remove_vm(vm_id: usize) -> Option<VMRef> {
    crate::manager::remove_existing_vm(vm_id)
}

/// Register a prepared VM in the AxVM runtime.
pub fn register_vm(vm: VMRef) -> bool {
    crate::manager::push_existing_vm(vm)
}
