mod config;
mod images;
mod vcpus;
mod vm_list;

use crate::hal::AxVMHalImpl;
use axvm::{AxVM, AxVMRef};

pub type VM = AxVM<AxVMHalImpl>;
pub type VMRef = AxVMRef<AxVMHalImpl>;

pub fn init() {
    // Initialize guest VM according to config file.
    config::init_guest_vms();

    // Setup vcpus, spawn axtask for VCpu.
    info!("Setting up vcpus...");
    for vm in vm_list::get_vm_list() {
        vcpus::setup_vm_vcpus(vm);
    }
}

pub fn start() {
    info!("VMM starting, booting VMs...");
    for vm in vm_list::get_vm_list() {
        match vm.boot() {
            Ok(_) => info!("VM[{}] boot success", vm.id()),
            Err(err) => warn!("VM[{}] boot failed, error {:?}", vm.id(), err),
        }
    }
}
