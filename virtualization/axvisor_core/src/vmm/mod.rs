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

pub mod config;
pub mod devices;
pub mod images;
pub mod timer;
pub mod vcpus;
pub mod vm_list;

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub mod fdt;

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err_type};
use ax_lazyinit::LazyInit;
use axvisor_api::{
    api_impl,
    time::TimeValue,
    types::{InterruptVector, VCpuId, VCpuSet, VMId},
    vmm as api_vmm,
};
pub use timer::init_percpu as init_timer_percpu;

/// The instantiated VM type.
pub type VM = axvm::AxVM;
/// The instantiated VM ref type (by `Arc`).
pub type VMRef = axvm::AxVMRef;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = axvm::AxVCpuRef;

static VMM: LazyInit<axvisor_api::task::WaitQueue> = LazyInit::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize the VMM.
///
/// This function creates the VM structures and sets up the primary VCpu for each VM.
pub fn init() {
    info!("Initializing VMM...");
    VMM.init_once(axvisor_api::task::WaitQueue::new());
    // Initialize guest VM according to config file.
    config::init_guest_vms();

    // Setup vCPUs, spawn an ax-task for the primary vCPU.
    info!("Setting up vcpus...");
    for vm in vm_list::get_vm_list() {
        vcpus::setup_vm_primary_vcpu(vm);
    }

    #[cfg(all(feature = "fs", target_arch = "x86_64"))]
    release_host_filesystem_for_guest_passthrough().expect(
        "Failed to release host filesystem before guest passthrough devices take ownership",
    );
}

#[cfg(all(feature = "fs", target_arch = "x86_64"))]
fn release_host_filesystem_for_guest_passthrough() -> AxResult {
    let has_conflicting_guest_ownership = vm_list::get_vm_list()
        .into_iter()
        .any(|vm| vm.has_host_fs_passthrough_conflict());
    if !has_conflicting_guest_ownership {
        return Ok(());
    }

    axvisor_api::host::release_host_filesystems()?;
    info!("Host filesystem cleanly unmounted before guest passthrough devices start");
    Ok(())
}

/// Start the VMM.
pub fn start() {
    info!("VMM starting, booting VMs...");
    for vm in vm_list::get_vm_list() {
        match vm.boot() {
            Ok(_) => {
                vcpus::notify_primary_vcpu(vm.id());
                RUNNING_VM_COUNT.fetch_add(1, Ordering::Release);
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

#[allow(unused_imports)]
pub use vcpus::with_vcpu_task;

/// Run a closure with the specified VM.
pub fn with_vm<T>(vm_id: usize, f: impl FnOnce(VMRef) -> T) -> Option<T> {
    let vm = vm_list::get_vm_by_id(vm_id)?;
    Some(f(vm))
}

/// Run a closure with the specified VM and vCPU.
pub fn with_vm_and_vcpu<T>(
    vm_id: usize,
    vcpu_id: usize,
    f: impl FnOnce(VMRef, VCpuRef) -> T,
) -> Option<T> {
    let vm = vm_list::get_vm_by_id(vm_id)?;
    let vcpu = vm.vcpu(vcpu_id)?;

    Some(f(vm, vcpu))
}

/// Run a closure with the specified VM and vCPU, with the guarantee that the closure will be
/// executed on the physical CPU where the vCPU is running, waiting, or queueing.
///
/// TODO: It seems necessary to disable scheduling when running the closure.
pub fn with_vm_and_vcpu_on_pcpu(
    vm_id: usize,
    vcpu_id: usize,
    f: impl FnOnce(VMRef, VCpuRef) + 'static,
) -> AxResult {
    // Disables preemption and IRQs to prevent the current task from being preempted or re-scheduled.
    let guard = ax_kernel_guard::NoPreemptIrqSave::new();

    let current_vm = axvisor_api::task::current_vm_id();
    let current_vcpu = axvisor_api::task::current_vcpu_id();

    // The target vCPU is the current task, execute the closure directly.
    if current_vm == vm_id && current_vcpu == vcpu_id {
        with_vm_and_vcpu(vm_id, vcpu_id, f).ok_or_else(|| ax_err_type!(NotFound))?;
        return Ok(());
    }

    // The target vCPU is not the current task, send an IPI to the target physical CPU.
    drop(guard);

    let _pcpu_id = vcpus::with_vcpu_task(vm_id, vcpu_id, |task| task.cpu_id())
        .ok_or_else(|| ax_err_type!(NotFound))?;

    ax_errno::ax_err!(
        Unsupported,
        "cross-CPU vCPU closure dispatch is not implemented"
    )
}

pub fn add_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_add(count, Ordering::Release);
}

pub fn sub_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_sub(count, Ordering::Release);
}

struct VmmIfImpl;

#[api_impl]
impl api_vmm::VmmIf for VmmIfImpl {
    fn vcpu_num(vm_id: VMId) -> Option<usize> {
        with_vm(vm_id, |vm| vm.vcpu_num())
    }

    fn active_vcpus(vm_id: VMId) -> Option<usize> {
        with_vm(vm_id, |vm| {
            let vcpu_num = vm.vcpu_num();
            if vcpu_num >= usize::BITS as usize {
                usize::MAX
            } else {
                (1usize << vcpu_num) - 1
            }
        })
    }

    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) {
        let _ = with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, move |_, vcpu| {
            vcpu.inject_interrupt(vector as usize).unwrap();
        });
    }

    fn inject_interrupt_to_cpus(vm_id: VMId, vcpu_set: VCpuSet, vector: InterruptVector) {
        for vcpu_id in &vcpu_set {
            Self::inject_interrupt(vm_id, vcpu_id, vector);
        }
    }

    fn register_timer(
        deadline: TimeValue,
        handler: alloc::boxed::Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> api_vmm::CancelToken {
        timer::register_timer(deadline.as_nanos() as u64, handler)
    }

    fn cancel_timer(token: api_vmm::CancelToken) {
        timer::cancel_timer(token)
    }
}
