mod config;
mod images;
#[allow(unused)] //TODO: remove this with "irq" feature.
mod timer;
mod vcpus;
mod vm_list;

use std::os::arceos::api::task::{self, AxWaitQueueHandle};

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::hal::{AxVCpuHalImpl, AxVMHalImpl};
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
            info!("a VM exited, current running VM count: {}", vm_count);
            vm_count == 0
        },
        None,
    );
}
