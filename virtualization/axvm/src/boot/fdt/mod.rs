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

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
mod create;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
mod device;
#[cfg(target_arch = "loongarch64")]
pub(crate) mod loongarch64;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
mod parser;
mod print;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
mod tree;

#[cfg(test)]
mod tree_tests;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use alloc::format;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use alloc::vec::Vec;

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
use ax_errno::AxResult;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_errno::ax_err_type;
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
use axvmconfig::{AxVMCrateConfig, VMBootProtocol};
// pub use print::print_fdt;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use create::update_fdt;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
pub use parser::*;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::boot::BootImageProvider;
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
use crate::config::AxVMConfig;

/// Guest DTB artifact produced or patched by the monitor before AxVM owns it.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
#[derive(Debug, Clone)]
pub struct GuestDtbImage {
    bytes: Vec<u8>,
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
impl GuestDtbImage {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Initialize LoongArch guest firmware resource handling.
#[cfg(target_arch = "loongarch64")]
pub fn init_guest_boot_resources() {
    crate::boot::guest_platform::loongarch64::init();
}

#[cfg(target_arch = "loongarch64")]
fn handle_uefi_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    crate::boot::guest_platform::loongarch64::prepare_uefi_fdt_config(vm_config, vm_create_config)
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

/// Prepare LoongArch guest firmware FDT facts for UEFI boot.
#[cfg(target_arch = "loongarch64")]
pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        handle_uefi_fdt_operations(vm_config, vm_create_config)?;
        return Ok(());
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
    provider: &dyn BootImageProvider,
) -> AxResult<Option<GuestDtbImage>> {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        handle_uefi_fdt_operations(vm_config, vm_create_config)?;
        return Ok(None);
    }

    let host_fdt_bytes = try_get_host_fdt();
    let mut guest_dtb = None;

    if let Some(host_fdt_bytes) = host_fdt_bytes {
        let host_fdt = fdt_edit::Fdt::from_bytes(host_fdt_bytes)
            .map_err(|e| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {e:#?}")))?;
        set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;

        if let Some(provided_dtb) =
            get_developer_provided_dtb(vm_config, vm_create_config, provider)?
        {
            info!("VM[{}] found DTB , parsing...", vm_config.id());
            reserve_excluded_device_ranges(vm_config, vm_create_config, &provided_dtb)?;
            guest_dtb = Some(GuestDtbImage::new(update_provided_fdt(
                &provided_dtb,
                Some(host_fdt_bytes),
                vm_create_config,
            )?));
        } else {
            info!(
                "VM[{}] DTB not found, generating based on the configuration file.",
                vm_config.id()
            );
            guest_dtb = Some(GuestDtbImage::new(setup_guest_fdt_from_vmm(
                host_fdt_bytes,
                vm_config,
                vm_create_config,
            )?));
        }
    } else if let Some(provided_dtb) =
        get_developer_provided_dtb(vm_config, vm_create_config, provider)?
    {
        info!("VM[{}] found DTB , parsing...", vm_config.id());
        reserve_excluded_device_ranges(vm_config, vm_create_config, &provided_dtb)?;
        guest_dtb = Some(GuestDtbImage::new(update_provided_fdt(
            &provided_dtb,
            None,
            vm_create_config,
        )?));
    } else {
        warn!(
            "VM[{}] no guest DTB provided; continuing without generated DTB",
            vm_config.id()
        );
    }

    // Overlay VM config with the given DTB.
    if let Some(dtb) = guest_dtb.as_ref().map(GuestDtbImage::as_bytes) {
        parse_reserved_memory_regions(vm_create_config, dtb)?;
        parse_passthrough_devices_address(vm_config, vm_create_config, dtb)?;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
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
    Ok(guest_dtb)
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn get_developer_provided_dtb(
    vm_cfg: &AxVMConfig,
    crate_config: &AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxResult<Option<Vec<u8>>> {
    match crate_config.kernel.image_location.as_deref() {
        Some("memory") => {
            let vm_images = provider
                .static_vm_images()
                .iter()
                .find(|&v| v.id == vm_cfg.id());

            if let Some(dtb) = vm_images.and_then(|images| images.dtb) {
                info!("DTB file in memory, size: 0x{:x}", dtb.len());
                return Ok(Some(dtb.to_vec()));
            }
        }
        #[cfg(any(feature = "fs", feature = "host-fs"))]
        Some("fs") => {
            if let Some(dtb_path) = &crate_config.kernel.dtb_path {
                let dtb_buffer = crate::boot::images::fs::read_full_image(dtb_path, provider)?;
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
