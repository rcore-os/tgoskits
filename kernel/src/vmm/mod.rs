mod hvc;
mod ivc;

pub mod config;
pub mod images;
pub mod timer;
pub mod vcpus;
pub mod vm_list;

#[cfg(target_arch = "aarch64")]
pub mod fdt;

use core::sync::atomic::{AtomicUsize, Ordering};
use std::os::arceos::{
    api::task::{self, AxWaitQueueHandle},
    modules::axtask,
};

use axerrno::{AxResult, ax_err_type};

use crate::{
    hal::{AxVCpuHalImpl, AxVMHalImpl},
    task::AsVCpuTask,
};
pub use timer::init_percpu as init_timer_percpu;

/// The instantiated VM type.
pub type VM = axvm::AxVM<AxVMHalImpl, AxVCpuHalImpl>;
/// The instantiated VM ref type (by `Arc`).
pub type VMRef = axvm::AxVMRef<AxVMHalImpl, AxVCpuHalImpl>;
/// The instantiated VCpu ref type (by `Arc`).
pub type VCpuRef = axvm::AxVCpuRef<AxVCpuHalImpl>;

static VMM: AxWaitQueueHandle = AxWaitQueueHandle::new();

/// The number of running VMs. This is used to determine when to exit the VMM.
static RUNNING_VM_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialize the VMM.
///
/// This function creates the VM structures and sets up the primary VCpu for each VM.
pub fn init() {
    info!("Initializing VMM...");
    // Initialize guest VM according to config file.
    config::init_guest_vms();

    // Setup vcpus, spawn axtask for primary VCpu.
    info!("Setting up vcpus...");
    for vm in vm_list::get_vm_list() {
        vcpus::setup_vm_primary_vcpu(vm);
    }
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
    task::ax_wait_queue_wait_until(
        &VMM,
        || {
            let vm_count = RUNNING_VM_COUNT.load(Ordering::Acquire);
            info!("a VM exited, current running VM count: {vm_count}");
            vm_count == 0
        },
        None,
    );
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
    let guard = kernel_guard::NoPreemptIrqSave::new();

    let current_vm = axtask::current().as_vcpu_task().vm().id();
    let current_vcpu = axtask::current().as_vcpu_task().vcpu.id();

    // The target vCPU is the current task, execute the closure directly.
    if current_vm == vm_id && current_vcpu == vcpu_id {
        with_vm_and_vcpu(vm_id, vcpu_id, f).unwrap(); // unwrap is safe here
        return Ok(());
    }

    // The target vCPU is not the current task, send an IPI to the target physical CPU.
    drop(guard);

    let _pcpu_id = vcpus::with_vcpu_task(vm_id, vcpu_id, |task| task.cpu_id())
        .ok_or_else(|| ax_err_type!(NotFound))?;

    unimplemented!();
    // use std::os::arceos::modules::axipi;
    // Ok(axipi::send_ipi_event_to_one(pcpu_id as usize, move || {
    // with_vm_and_vcpu_on_pcpu(vm_id, vcpu_id, f);
    // }))
}

pub fn add_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_add(count, Ordering::Release);
}

pub fn sub_running_vm_count(count: usize) {
    RUNNING_VM_COUNT.fetch_sub(count, Ordering::Release);
}
