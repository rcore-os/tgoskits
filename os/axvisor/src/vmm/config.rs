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

use ax_errno::{AxResult, ax_err_type};
use axaddrspace::GuestPhysAddr;
use axvm::{
    VMMemoryRegion,
    config::{
        AxVMConfig, AxVMCrateConfig, VMBootProtocol, VmMemMappingType, adjusted_kernel_load_gpa,
    },
};
use core::alloc::Layout;

use crate::vmm::{VM, images::ImageLoader, vm_list::push_vm};

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
use crate::vmm::fdt::*;

use alloc::sync::Arc;

#[allow(dead_code)]
pub mod vmcfg {
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Default static VM configs. Used when no VM config is provided.
    pub fn default_static_vm_configs() -> Vec<&'static str> {
        vec![]
    }

    /// Read VM configs from filesystem
    #[cfg(feature = "fs")]
    pub fn filesystem_vm_configs() -> Vec<String> {
        use ax_std::fs;
        use ax_std::io::{BufReader, Read};

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

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!("Failed to read config directory entry: {e:?}");
                    continue;
                }
            };
            let path = entry.path();
            // Check if the file has a .toml extension
            let path_str = path.as_str();
            debug!("Considering file: {}", path_str);
            if path_str.ends_with(".toml") {
                let toml_file = match fs::File::open(path_str) {
                    Ok(file) => file,
                    Err(e) => {
                        error!("Failed to open config file {}: {:?}", path_str, e);
                        continue;
                    }
                };
                let file_size = match toml_file.metadata() {
                    Ok(metadata) => metadata.len() as usize,
                    Err(e) => {
                        error!("Failed to get config file {} metadata: {:?}", path_str, e);
                        continue;
                    }
                };

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
                        let content = match String::from_utf8(buffer) {
                            Ok(content) => content,
                            Err(e) => {
                                error!("Config file {} is not valid UTF-8: {:?}", path_str, e);
                                continue;
                            }
                        };

                        match axvm::config::AxVMCrateConfig::from_toml(&content) {
                            Ok(_) => {
                                configs.push(content);
                                info!(
                                    "TOML config: {} is valid, start the virtual machine directly now. ",
                                    path_str
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "File {} does not contain a valid VM config: {:?}",
                                    path_str, e
                                );
                            }
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
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    ))]
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
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    ))]
    {
        init_dtb_cache();
    }

    // First try to get configs from filesystem if fs feature is enabled
    let mut gvm_raw_configs = vmcfg::filesystem_vm_configs();

    // If no filesystem configs found, fallback to static configs
    if gvm_raw_configs.is_empty() {
        let static_configs = vmcfg::static_vm_configs();
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
    #[allow(unused_mut)]
    let mut vm_create_config = AxVMCrateConfig::from_toml(raw_cfg)
        .map_err(|e| ax_err_type!(InvalidData, format!("Failed to resolve VM config: {e:?}")))?;

    if let Some(linux) = super::images::get_image_header(&vm_create_config) {
        debug!(
            "VM[{}] Linux header: {:#x?}",
            vm_create_config.base.id, linux
        );
    }

    #[allow(unused_mut)]
    let mut vm_config = AxVMConfig::from(vm_create_config.clone());

    // Handle FDT-related operations for architectures that boot guests with DTB.
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    ))]
    handle_fdt_operations(&mut vm_config, &mut vm_create_config)?;

    #[cfg(target_arch = "x86_64")]
    let skip_guest_address_adjustment = x86_linux_direct_boot_config(&vm_create_config);
    #[cfg(not(target_arch = "x86_64"))]
    let skip_guest_address_adjustment = false;

    // info!("after parse_vm_interrupt, crate VM[{}] with config: {:#?}", vm_config.id(), vm_config);
    info!("Creating VM[{}] {:?}", vm_config.id(), vm_config.name());

    // Create VM.
    let vm = VM::new(vm_config)
        .map_err(|e| ax_err_type!(InvalidData, format!("Failed to create VM: {e:?}")))?;
    let vm_id = vm.id();

    vm_alloc_memory_regions(&vm_create_config, &vm)?;

    let main_mem = vm
        .memory_regions()
        .first()
        .cloned()
        .ok_or_else(|| ax_err_type!(InvalidData, "VM must have at least one memory region"))?;

    if !skip_guest_address_adjustment {
        config_guest_address(
            &vm,
            &main_mem,
            vm_create_config.kernel.effective_boot_protocol(),
        );
    }

    // Load corresponding images for VM.
    info!("VM[{}] created success, loading images...", vm.id());

    let mut loader = ImageLoader::new(main_mem, vm_create_config, vm.clone());
    loader.load()?;

    vm.init()
        .map_err(|e| ax_err_type!(InvalidData, format!("VM[{}] setup failed: {e:?}", vm.id())))?;

    vm.set_vm_status(axvm::VMStatus::Loaded);
    push_vm(vm);

    Ok(vm_id)
}

fn config_guest_address(vm: &VM, main_memory: &VMMemoryRegion, boot_protocol: VMBootProtocol) {
    vm.with_config(|config| {
        if let Some(kernel_addr) = adjusted_kernel_load_gpa(
            main_memory,
            boot_protocol,
            config.image_config.bios_load_gpa,
        ) {
            debug!(
                "Adjusting kernel load address from {:#x} to {:#x}",
                config.image_config.kernel_load_gpa, kernel_addr
            );
            config.relocate_kernel_image(kernel_addr);
        }
    });
}

#[cfg(target_arch = "x86_64")]
fn x86_linux_direct_boot_config(config: &AxVMCrateConfig) -> bool {
    crate::vmm::images::is_x86_linux_image_config(config)
}

fn vm_alloc_memory_regions(vm_create_config: &AxVMCrateConfig, vm: &VM) -> AxResult {
    const MB: usize = 1024 * 1024;
    const ALIGN: usize = 2 * MB;

    let make_layout = |memory: &axvm::config::VmMemConfig| {
        Layout::from_size_align(memory.size, ALIGN).map_err(|e| {
            ax_err_type!(
                InvalidInput,
                format!("Invalid VM memory layout {:?}: {e:?}", memory)
            )
        })
    };

    for memory in &vm_create_config.kernel.memory_regions {
        match memory.map_type {
            VmMemMappingType::MapAlloc => {
                vm.alloc_memory_region(make_layout(memory)?, Some(GuestPhysAddr::from(memory.gpa)))
                    .map_err(|e| {
                        ax_err_type!(
                            NoMemory,
                            format!("Failed to allocate memory region for VM: {e:?}")
                        )
                    })?;
            }
            VmMemMappingType::MapIdentical => {
                vm.alloc_memory_region(make_layout(memory)?, None)
                    .map_err(|e| {
                        ax_err_type!(
                            NoMemory,
                            format!("Failed to allocate memory region for VM: {e:?}")
                        )
                    })?;
            }
            VmMemMappingType::MapReserved => {
                debug!("VM[{}] map same region: {:#x?}", vm.id(), memory);
                vm.map_reserved_memory_region(
                    make_layout(memory)?,
                    Some(GuestPhysAddr::from(memory.gpa)),
                )
                .map_err(|e| {
                    ax_err_type!(
                        NoMemory,
                        format!("Failed to map memory region for VM: {e:?}")
                    )
                })?;
            }
        }
    }
    Ok(())
}
