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

use alloc::{format, sync::Arc};
use core::alloc::Layout;
#[cfg(all(
    feature = "fs",
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxResult, ax_err_type};
use axvm::{
    AxVM, AxVMRef, GuestPhysAddr, VMMemoryRegion,
    config::{
        AxVCpuConfig, AxVMConfig, AxVMConfigParams, PhysCpuList, RamdiskInfo, VMBootProtocol,
        VMImageConfig, adjusted_kernel_load_gpa,
    },
};
use axvmconfig::{AxVMCrateConfig, VMType, VmMemConfig, VmMemMappingType};

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
use crate::fdt::*;
use crate::images::ImageLoader;

/// Default BIOS load GPA for x86_64 built-in BIOS.
#[cfg(target_arch = "x86_64")]
const DEFAULT_X86_BIOS_LOAD_GPA: usize = 0x8000;

#[cfg(all(
    feature = "fs",
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
static HOST_FILESYSTEM_RELEASE_REQUIRED: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub mod vmcfg {
    use alloc::{string::String, vec, vec::Vec};

    /// Default static VM configs. Used when no VM config is provided.
    pub fn default_static_vm_configs() -> Vec<&'static str> {
        vec![]
    }

    /// Read VM configs from filesystem
    #[cfg(feature = "fs")]
    pub fn filesystem_vm_configs() -> Vec<String> {
        let config_dir = "/guest/vm_default";
        crate::manager::AxvmManager::filesystem_vm_configs(config_dir)
            .into_iter()
            .filter_map(
                |content| match axvmconfig::AxVMCrateConfig::from_toml(&content) {
                    Ok(_) => Some(content),
                    Err(e) => {
                        warn!("Filesystem VM config is invalid: {:?}", e);
                        None
                    }
                },
            )
            .collect()
    }

    /// Fallback function for when "fs" feature is not enabled
    #[cfg(not(feature = "fs"))]
    pub fn filesystem_vm_configs() -> Vec<String> {
        Vec::new()
    }

    include!(concat!(env!("OUT_DIR"), "/vm_configs.rs"));
}

pub fn get_vm_dtb_arc(_vm_cfg: &AxVMConfig) -> Option<Arc<[u8]>> {
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
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

    #[cfg(all(
        feature = "fs",
        any(target_arch = "x86_64", target_arch = "loongarch64")
    ))]
    let release_host_filesystem = vm_config_needs_host_filesystem_release(&vm_create_config);

    if let Some(linux) = super::images::get_image_header(&vm_create_config) {
        debug!(
            "VM[{}] Linux header: {:#x?}",
            vm_create_config.base.id, linux
        );
    }

    #[allow(unused_mut)]
    let mut vm_config = build_axvm_config(&vm_create_config);

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
    let vm = AxVM::new(vm_config)
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
    if !axvm::register_vm(vm) {
        return Err(ax_err_type!(
            AlreadyExists,
            format!("VM[{vm_id}] already exists")
        ));
    }
    #[cfg(target_arch = "loongarch64")]
    crate::manager::register_loongarch_passthrough_irq_routes(vm_id);

    #[cfg(all(
        feature = "fs",
        any(target_arch = "x86_64", target_arch = "loongarch64")
    ))]
    if release_host_filesystem {
        HOST_FILESYSTEM_RELEASE_REQUIRED.store(true, Ordering::Release);
    }

    Ok(vm_id)
}

pub(crate) fn build_axvm_config(cfg: &AxVMCrateConfig) -> AxVMConfig {
    AxVMConfig::new(AxVMConfigParams {
        id: cfg.base.id,
        name: cfg.base.name.clone(),
        vm_type: VMType::from(cfg.base.vm_type),
        phys_cpu_ls: PhysCpuList::new(
            cfg.base.cpu_num,
            cfg.base.phys_cpu_ids.clone(),
            cfg.base.phys_cpu_sets.clone(),
        ),
        cpu_config: AxVCpuConfig {
            bsp_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
            ap_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
            #[cfg(target_arch = "loongarch64")]
            boot_args: [0; 3],
            #[cfg(target_arch = "loongarch64")]
            boot_stack_top: 0,
            #[cfg(target_arch = "loongarch64")]
            firmware_boot: cfg.kernel.effective_boot_protocol() == VMBootProtocol::Uefi,
        },
        image_config: VMImageConfig {
            kernel_load_gpa: GuestPhysAddr::from(cfg.kernel.kernel_load_addr),
            loaded_from_filesystem: cfg.kernel.image_location.as_deref() == Some("fs"),
            bios_load_gpa: configured_bios_load_gpa(cfg),
            dtb_load_gpa: cfg.kernel.dtb_load_addr.map(GuestPhysAddr::from),
            ramdisk: cfg.kernel.ramdisk_load_addr.map(|addr| RamdiskInfo {
                load_gpa: GuestPhysAddr::from(addr),
                size: None,
            }),
        },
        emu_devices: cfg.devices.emu_devices.clone(),
        pass_through_devices: cfg.devices.passthrough_devices.clone(),
        excluded_devices: cfg.devices.excluded_devices.clone(),
        pass_through_addresses: cfg.devices.passthrough_addresses.clone(),
        interrupt_mode: cfg.devices.interrupt_mode,
    })
}

fn configured_bios_load_gpa(cfg: &AxVMCrateConfig) -> Option<GuestPhysAddr> {
    if !cfg.kernel.enable_bios {
        return None;
    }

    if let Some(addr) = cfg.kernel.bios_load_addr {
        return Some(GuestPhysAddr::from(addr));
    }

    #[cfg(target_arch = "x86_64")]
    if cfg.kernel.boot_firmware_path().is_none()
        && cfg.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
    {
        return Some(GuestPhysAddr::from(DEFAULT_X86_BIOS_LOAD_GPA));
    }

    None
}

#[cfg(all(
    feature = "fs",
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
fn vm_config_needs_host_filesystem_release(config: &AxVMCrateConfig) -> bool {
    let has_passthrough = !config.devices.passthrough_devices.is_empty()
        || !config.devices.passthrough_addresses.is_empty();
    has_passthrough && config.kernel.image_location.as_deref() == Some("fs")
}

#[cfg(all(
    feature = "fs",
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
pub fn host_filesystem_release_required() -> bool {
    HOST_FILESYSTEM_RELEASE_REQUIRED.load(Ordering::Acquire)
}

fn config_guest_address(vm: &AxVMRef, main_memory: &VMMemoryRegion, boot_protocol: VMBootProtocol) {
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
    crate::images::is_x86_linux_image_config(config)
}

fn vm_alloc_memory_regions(vm_create_config: &AxVMCrateConfig, vm: &AxVMRef) -> AxResult {
    const MB: usize = 1024 * 1024;
    const ALIGN: usize = 2 * MB;

    let make_layout = |memory: &VmMemConfig| {
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
