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

//! FDT (Flattened Device Tree) processing module for AxVisor.
//!
//! This module provides functionality for parsing and processing device tree blobs,
//! including CPU configuration, passthrough device detection, and FDT generation.

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod create;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod device;
#[cfg(target_arch = "loongarch64")]
pub(crate) mod loongarch64;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod parser;
mod print;
pub(crate) mod vm_fdt;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use alloc::format;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use alloc::{collections::BTreeMap, vec::Vec};

use ax_errno::AxResult;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_errno::ax_err_type;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_lazyinit::LazyInit;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use fdt_parser::Fdt;
// pub use print::print_fdt;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use create::update_fdt;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use device::build_all_node_paths;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use parser::*;

use axvm::config::AxVMConfig;
use axvmconfig::{AxVMCrateConfig, VMBootProtocol};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::config::{get_vm_dtb_arc, vmcfg};

// DTB cache for generated device trees
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
static GENERATED_DTB_CACHE: LazyInit<Mutex<BTreeMap<usize, Vec<u8>>>> = LazyInit::new();

/// Initialize the DTB cache
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn init_dtb_cache() {
    GENERATED_DTB_CACHE.init_once(Mutex::new(BTreeMap::new()));
}

/// Initialize LoongArch guest firmware resource handling.
#[cfg(target_arch = "loongarch64")]
pub fn init_dtb_cache() {
    crate::guest_platform::loongarch64::init();
}

/// Get reference to the DTB cache
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn dtb_cache() -> &'static Mutex<BTreeMap<usize, Vec<u8>>> {
    GENERATED_DTB_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Generate guest FDT cache the result
/// # Return Value
/// Returns the generated DTB data and stores it in the global cache
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn crate_guest_fdt_with_cache(dtb_data: Vec<u8>, crate_config: &AxVMCrateConfig) {
    // Store data in global cache
    let mut cache_lock = dtb_cache().lock();
    cache_lock.insert(crate_config.base.id, dtb_data);
}

#[cfg(target_arch = "loongarch64")]
fn handle_uefi_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    crate::guest_platform::loongarch64::prepare_uefi_fdt_config(vm_config, vm_create_config)
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn handle_uefi_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    info!(
        "VM[{}] uses UEFI boot protocol, skipping guest DTB handling",
        vm_config.id()
    );
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
    Ok(())
}

/// Handle all FDT-related operations for guest architectures that boot with DTB.
#[cfg(target_arch = "loongarch64")]
pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        return handle_uefi_fdt_operations(vm_config, vm_create_config);
    }

    ax_errno::ax_err!(
        Unsupported,
        "LoongArch AxVisor guests currently require UEFI boot"
    )
}

/// Handle all FDT-related operations for guest architectures that boot with DTB.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        return handle_uefi_fdt_operations(vm_config, vm_create_config);
    }

    let host_fdt_bytes = try_get_host_fdt();

    if let Some(host_fdt_bytes) = host_fdt_bytes {
        let host_fdt = Fdt::from_bytes(host_fdt_bytes)
            .map_err(|e| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {e:#?}")))?;
        set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;

        if let Some(provided_dtb) = get_developer_provided_dtb(vm_config, vm_create_config)? {
            info!("VM[{}] found DTB , parsing...", vm_config.id());
            update_provided_fdt(&provided_dtb, host_fdt_bytes, vm_create_config)?;
        } else {
            info!(
                "VM[{}] DTB not found, generating based on the configuration file.",
                vm_config.id()
            );
            setup_guest_fdt_from_vmm(host_fdt_bytes, vm_config, vm_create_config)?;
        }
    } else if let Some(provided_dtb) = get_developer_provided_dtb(vm_config, vm_create_config)? {
        info!("VM[{}] found DTB , parsing...", vm_config.id());
        update_provided_fdt(&provided_dtb, &[], vm_create_config)?;
    } else {
        warn!(
            "VM[{}] no guest DTB provided; continuing without generated DTB",
            vm_config.id()
        );
    }

    // Overlay VM config with the given DTB.
    if let Some(dtb_arc) = get_vm_dtb_arc(vm_config) {
        let dtb = dtb_arc.as_ref();
        parse_reserved_memory_regions(vm_create_config, dtb)?;
        parse_passthrough_devices_address(vm_config, vm_create_config, dtb)?;
        #[cfg(target_arch = "aarch64")]
        parse_vm_interrupt(vm_config, dtb)?;
    } else {
        error!(
            "VM[{}] DTB not found in memory, skipping...",
            vm_config.id()
        );
        let unresolved_passthrough_devices = vm_config
            .pass_through_devices()
            .iter()
            .filter(|device| device.length == 0)
            .cloned()
            .collect::<Vec<_>>();
        if !unresolved_passthrough_devices.is_empty() {
            warn!(
                "VM[{}] clearing {} unresolved passthrough discovery device(s)",
                vm_config.id(),
                unresolved_passthrough_devices.len()
            );
            for device in unresolved_passthrough_devices {
                vm_config.remove_pass_through_device(device);
            }
        }
        vm_config.clear_dtb_load_gpa();
        vm_create_config.kernel.dtb_load_addr = None;
    }
    Ok(())
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn get_developer_provided_dtb(
    vm_cfg: &AxVMConfig,
    crate_config: &AxVMCrateConfig,
) -> AxResult<Option<Vec<u8>>> {
    match crate_config.kernel.image_location.as_deref() {
        Some("memory") => {
            let vm_imags = vmcfg::get_memory_images()
                .iter()
                .find(|&v| v.id == vm_cfg.id());

            if let Some(dtb) = vm_imags.and_then(|images| images.dtb) {
                info!("DTB file in memory, size: 0x{:x}", dtb.len());
                return Ok(Some(dtb.to_vec()));
            }
        }
        #[cfg(feature = "fs")]
        Some("fs") => {
            if let Some(dtb_path) = &crate_config.kernel.dtb_path {
                let dtb_buffer = crate::images::fs::read_full_image(dtb_path)?;
                info!("DTB file in fs, size: 0x{:x}", dtb_buffer.len());
                return Ok(Some(dtb_buffer));
            }
        }
        _ => {
            return ax_errno::ax_err!(
                InvalidInput,
                "Unsupported image_location; use \"memory\" or enable fs feature for \"fs\""
            );
        }
    }
    Ok(None)
}
