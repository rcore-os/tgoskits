use axvm::config::{AxVMConfig, AxVMCrateConfig};

use crate::vmm::{VM, images::load_vm_images, vm_list::push_vm};

#[allow(clippy::module_inception)]
pub mod config {
    use alloc::vec::Vec;

    /// Default static VM configs. Used when no VM config is provided.
    #[allow(dead_code)]
    pub fn default_static_vm_configs() -> Vec<&'static str> {
        vec![
            #[cfg(target_arch = "x86_64")]
            core::include_str!("../../configs/vms/nimbos-x86_64.toml"),
            #[cfg(target_arch = "aarch64")]
            core::include_str!("../../configs/vms/nimbos-aarch64.toml"),
            #[cfg(target_arch = "riscv64")]
            core::include_str!("../../configs/vms/nimbos-riscv64.toml"),
        ]
    }

    include!(concat!(env!("OUT_DIR"), "/vm_configs.rs"));
}

pub fn init_guest_vms() {
    let gvm_raw_configs = config::static_vm_configs();

    for raw_cfg_str in gvm_raw_configs {
        let vm_create_config =
            AxVMCrateConfig::from_toml(raw_cfg_str).expect("Failed to resolve VM config");
        let vm_config = AxVMConfig::from(vm_create_config.clone());

        info!("Creating VM[{}] {:?}", vm_config.id(), vm_config.name());

        // Create VM.
        let vm = VM::new(vm_config).expect("Failed to create VM");
        push_vm(vm.clone());

        // Load corresponding images for VM.
        info!("VM[{}] created success, loading images...", vm.id());
        load_vm_images(vm_create_config, vm.clone()).expect("Failed to load VM images");
    }
}
