use axvm::config::{AxVMConfig, AxVMCrateConfig};

use crate::vmm::{images::load_vm_images, vm_list::push_vm, VM};

pub fn init_guest_vms() {
    // Config file for guest VM should be read into memory in a more flexible way.
    // FIXME: remove this hardcode.
    let gvm_raw_configs = vec![
        #[cfg(target_arch = "x86_64")]
        core::include_str!("../../configs/nimbos-x86.toml"),
        #[cfg(target_arch = "aarch64")]
        core::include_str!("../../configs/nimbos-aarch64.toml"),
        #[cfg(target_arch = "riscv64")]
        core::include_str!("../../configs/nimbos-riscv64.toml"),
    ];

    for raw_cfg_str in gvm_raw_configs {
        let vm_create_config =
            AxVMCrateConfig::from_toml(raw_cfg_str).expect("Failed to resolve VM config");
        let vm_config = AxVMConfig::from(vm_create_config.clone());

        // Create VM.
        let vm = VM::new(vm_config).expect("Failed to create VM");
        push_vm(vm.clone());

        // Load corresponding images for VM.
        info!("VM[{}] created success, loading images...", vm.id());
        load_vm_images(vm_create_config, vm.clone()).expect("Failed to load VM images");
    }
}
