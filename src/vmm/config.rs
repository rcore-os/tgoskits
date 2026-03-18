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

use axaddrspace::GuestPhysAddr;
use axerrno::AxResult;
use axvm::{
    VMMemoryRegion,
    config::{AxVMConfig, AxVMCrateConfig, VmMemMappingType},
};
use core::alloc::Layout;

use crate::vmm::{VM, images::ImageLoader, vm_list::push_vm};

#[cfg(target_arch = "aarch64")]
use crate::vmm::fdt::*;

use alloc::sync::Arc;

#[allow(clippy::module_inception, dead_code)]
pub mod config {
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Default static VM configs. Used when no VM config is provided.
    pub fn default_static_vm_configs() -> Vec<&'static str> {
        vec![]
    }

    /// Read VM configs from filesystem
    #[cfg(feature = "fs")]
    pub fn filesystem_vm_configs() -> Vec<String> {
        use axstd::fs;
        use axstd::io::{BufReader, Read};

        let config_dir = "/guest/vm_default";

        let mut configs = Vec::new();

        debug!("Read VM config files from filesystem.");

        let entries = match fs::read_dir(config_dir) {
            Ok(entries) => {
                info!("Find dir: {}", config_dir);
                entries
            }
            Err(_e) => {
                info!("NOT find dir: {} in filesystem", config_dir);
                return configs;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            // Check if the file has a .toml extension
            let path_str = path.as_str();
            debug!("Considering file: {}", path_str);
            if path_str.ends_with(".toml") {
                let toml_file = fs::File::open(path_str).expect("Failed to open file");
                let file_size = toml_file
                    .metadata()
                    .expect("Failed to get file metadata")
                    .len() as usize;

                info!("File {} size: {}", path_str, file_size);

                if file_size == 0 {
                    warn!("File {} is empty", path_str);
                    continue;
                }

                let mut file = BufReader::new(toml_file);
                let mut buffer = vec![0u8; file_size];
                match file.read_exact(&mut buffer) {
                    Ok(()) => {
                        debug!(
                            "Successfully read config file {} as bytes, size: {}",
                            path_str,
                            buffer.len()
                        );
                        // Convert to string
                        let content = alloc::string::String::from_utf8(buffer)
                            .expect("Failed to convert bytes to UTF-8 string");

                        if content.contains("[base]")
                            && content.contains("[kernel]")
                            && content.contains("[devices]")
                        {
                            configs.push(content);
                            info!(
                                "TOML config: {} is valid, start the virtual machine directly now. ",
                                path_str
                            );
                        } else {
                            warn!(
                                "File {} does not appear to contain valid VM config structure",
                                path_str
                            );
                        }
                    }
                    Err(e) => {
                        error!("Failed to read file {}: {:?}", path_str, e);
                    }
                }
            }
        }

        configs
    }

    /// Fallback function for when "fs" feature is not enabled
    #[cfg(not(feature = "fs"))]
    pub fn filesystem_vm_configs() -> Vec<String> {
        Vec::new()
    }

    include!(concat!(env!("OUT_DIR"), "/vm_configs.rs"));
}

pub fn get_vm_dtb_arc(_vm_cfg: &AxVMConfig) -> Option<Arc<[u8]>> {
    #[cfg(target_arch = "aarch64")]
    {
        let cache_lock = dtb_cache().lock();
        if let Some(dtb) = cache_lock.get(&_vm_cfg.id()) {
            return Some(Arc::from(dtb.as_slice()));
        }
    }
    None
}

pub fn init_guest_vms() {
    // Initialize the DTB cache in the fdt module
    #[cfg(target_arch = "aarch64")]
    {
        init_dtb_cache();
    }

    // First try to get configs from filesystem if fs feature is enabled
    let mut gvm_raw_configs = config::filesystem_vm_configs();

    // If no filesystem configs found, fallback to static configs
    if gvm_raw_configs.is_empty() {
        let static_configs = config::static_vm_configs();
        if static_configs.is_empty() {
            info!("Static VM configs are empty.");
            info!("Now axvisor will entry the shell...");
        } else {
            info!("Using static VM configs.");
        }
        // Convert static configs to String type
        gvm_raw_configs.extend(static_configs.into_iter().map(|s| s.into()));
    }

    for raw_cfg_str in gvm_raw_configs {
        debug!("Initializing guest VM with config: {:#?}", raw_cfg_str);
        if let Err(e) = init_guest_vm(&raw_cfg_str) {
            error!("Failed to initialize guest VM: {e:?}");
        }
    }
}

pub fn init_guest_vm(raw_cfg: &str) -> AxResult<usize> {
    let vm_create_config =
        AxVMCrateConfig::from_toml(raw_cfg).expect("Failed to resolve VM config");

    if let Some(linux) = super::images::get_image_header(&vm_create_config) {
        debug!(
            "VM[{}] Linux header: {:#x?}",
            vm_create_config.base.id, linux
        );
    }

    #[cfg(target_arch = "aarch64")]
    let mut vm_config = AxVMConfig::from(vm_create_config.clone());

    #[cfg(not(target_arch = "aarch64"))]
    let vm_config = AxVMConfig::from(vm_create_config.clone());

    // Handle FDT-related operations for aarch64
    #[cfg(target_arch = "aarch64")]
    handle_fdt_operations(&mut vm_config, &vm_create_config);

    // info!("after parse_vm_interrupt, crate VM[{}] with config: {:#?}", vm_config.id(), vm_config);
    info!("Creating VM[{}] {:?}", vm_config.id(), vm_config.name());

    // Create VM.
    let vm = VM::new(vm_config).expect("Failed to create VM");
    let vm_id = vm.id();
    push_vm(vm.clone());

    vm_alloc_memorys(&vm_create_config, &vm);

    let main_mem = vm
        .memory_regions()
        .first()
        .cloned()
        .expect("VM must have at least one memory region");

    config_guest_address(&vm, &main_mem);

    // Load corresponding images for VM.
    info!("VM[{}] created success, loading images...", vm.id());

    let mut loader = ImageLoader::new(main_mem, vm_create_config, vm.clone());
    loader.load().expect("Failed to load VM images");

    if let Err(e) = vm.init() {
        panic!("VM[{}] setup failed: {:?}", vm.id(), e);
    }

    vm.set_vm_status(axvm::VMStatus::Loaded);

    Ok(vm_id)
}

fn config_guest_address(vm: &VM, main_memory: &VMMemoryRegion) {
    const MB: usize = 1024 * 1024;
    vm.with_config(|config| {
        if main_memory.is_identical() {
            debug!(
                "Adjusting kernel load address from {:#x} to {:#x}",
                config.image_config.kernel_load_gpa, main_memory.gpa
            );
            let mut kernel_addr = main_memory.gpa;
            if config.image_config.bios_load_gpa.is_some() {
                kernel_addr += MB * 2; // leave 2MB for BIOS
            }

            config.image_config.kernel_load_gpa = kernel_addr;
            config.cpu_config.bsp_entry = kernel_addr;
            config.cpu_config.ap_entry = kernel_addr;
        }
    });
}

fn vm_alloc_memorys(vm_create_config: &AxVMCrateConfig, vm: &VM) {
    const MB: usize = 1024 * 1024;
    const ALIGN: usize = 2 * MB;

    for memory in &vm_create_config.kernel.memory_regions {
        match memory.map_type {
            VmMemMappingType::MapAlloc => {
                vm.alloc_memory_region(
                    Layout::from_size_align(memory.size, ALIGN).unwrap(),
                    Some(GuestPhysAddr::from(memory.gpa)),
                )
                .expect("Failed to allocate memory region for VM");
            }
            VmMemMappingType::MapIdentical => {
                vm.alloc_memory_region(Layout::from_size_align(memory.size, ALIGN).unwrap(), None)
                    .expect("Failed to allocate memory region for VM");
            }
            VmMemMappingType::MapReserved => {
                info!("VM[{}] map same region: {:#x?}", vm.id(), memory);
                let layout = Layout::from_size_align(memory.size, ALIGN).unwrap();
                vm.map_reserved_memory_region(layout, Some(GuestPhysAddr::from(memory.gpa)))
                    .expect("Failed to map memory region for VM");
            }
        }
    }
}
