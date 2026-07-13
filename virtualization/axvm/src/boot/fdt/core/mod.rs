//! Architecture-neutral guest device-tree preparation.

use alloc::{format, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use axvmconfig::{AxVMCrateConfig, VMBootProtocol};

use crate::{
    boot::{BootImageProvider, fdt::GuestDtbImage},
    config::AxVMConfig,
};

pub(crate) mod create;
mod device;
mod parser;
mod policy;
mod print;
pub(crate) mod tree;

#[cfg(test)]
mod tree_tests;

pub use create::{patch_guest_fdt_for_runtime, update_fdt};
pub use parser::*;
pub use policy::GuestFdtPolicy;

pub fn prepare_dtb_guest(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxResult<Option<GuestDtbImage>> {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        skip_guest_dtb(vm_config, vm_create_config);
        return Ok(None);
    }

    let host_fdt_bytes = try_get_host_fdt();
    let guest_dtb = build_guest_dtb(vm_config, vm_create_config, provider, host_fdt_bytes)?;
    enrich_guest_config(vm_config, vm_create_config, guest_dtb.as_ref())?;
    Ok(guest_dtb)
}

pub(crate) fn selected_guest_fdt_policy() -> GuestFdtPolicy {
    super::guest_fdt_policy()
}

fn skip_guest_dtb(vm_config: &mut AxVMConfig, vm_create_config: &mut AxVMCrateConfig) {
    info!(
        "VM[{}] uses UEFI boot protocol, skipping guest DTB handling",
        vm_config.id()
    );
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
}

fn build_guest_dtb(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
    provider: &dyn BootImageProvider,
    host_fdt_bytes: Option<&'static [u8]>,
) -> AxResult<Option<GuestDtbImage>> {
    let provided_dtb = get_developer_provided_dtb(vm_config, vm_create_config, provider)?;

    match (host_fdt_bytes, provided_dtb) {
        (Some(host_bytes), Some(provided)) => {
            let host_fdt = parse_host_fdt(host_bytes)?;
            set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;
            info!("VM[{}] found DTB, parsing...", vm_config.id());
            reserve_excluded_device_ranges(vm_config, vm_create_config, &provided)?;
            update_provided_fdt(&provided, Some(host_bytes), vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (Some(host_bytes), None) => {
            let host_fdt = parse_host_fdt(host_bytes)?;
            set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config)?;
            info!(
                "VM[{}] DTB not found, generating from the VM configuration",
                vm_config.id()
            );
            setup_guest_fdt_from_vmm(host_bytes, vm_config, vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (None, Some(provided)) => {
            info!("VM[{}] found DTB, parsing...", vm_config.id());
            reserve_excluded_device_ranges(vm_config, vm_create_config, &provided)?;
            update_provided_fdt(&provided, None, vm_create_config)
                .map(GuestDtbImage::new)
                .map(Some)
        }
        (None, None) => {
            warn!(
                "VM[{}] no guest DTB provided; continuing without generated DTB",
                vm_config.id()
            );
            Ok(None)
        }
    }
}

fn parse_host_fdt(host_fdt_bytes: &'static [u8]) -> AxResult<fdt_edit::Fdt> {
    fdt_edit::Fdt::from_bytes(host_fdt_bytes)
        .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {err:#?}")))
}

fn enrich_guest_config(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
    guest_dtb: Option<&GuestDtbImage>,
) -> AxResult {
    let Some(dtb) = guest_dtb.map(GuestDtbImage::as_bytes) else {
        clear_unresolved_dtb_config(vm_config, vm_create_config);
        return Ok(());
    };

    parse_reserved_memory_regions(vm_create_config, dtb)?;
    parse_passthrough_devices_address(vm_config, vm_create_config, dtb)?;
    parse_vm_interrupt(vm_config, dtb)
}

fn clear_unresolved_dtb_config(vm_config: &mut AxVMConfig, vm_create_config: &mut AxVMCrateConfig) {
    error!(
        "VM[{}] DTB not found in memory, skipping...",
        vm_config.id()
    );
    let unresolved_devices = vm_config
        .pass_through_devices()
        .iter()
        .filter(|device| device.length == 0)
        .cloned()
        .collect::<Vec<_>>();
    if !unresolved_devices.is_empty() {
        warn!(
            "VM[{}] clearing {} unresolved passthrough discovery device(s)",
            vm_config.id(),
            unresolved_devices.len()
        );
        for device in unresolved_devices {
            vm_config.remove_pass_through_device(device);
        }
    }
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
}

fn get_developer_provided_dtb(
    vm_config: &AxVMConfig,
    crate_config: &AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxResult<Option<Vec<u8>>> {
    match crate_config.kernel.image_location.as_deref() {
        Some("memory") => Ok(provider
            .static_vm_images()
            .iter()
            .find(|image| image.id == vm_config.id())
            .and_then(|images| images.dtb)
            .map(|dtb| {
                info!("DTB file in memory, size: 0x{:x}", dtb.len());
                dtb.to_vec()
            })),
        #[cfg(any(feature = "fs", feature = "host-fs"))]
        Some("fs") => crate_config
            .kernel
            .dtb_path
            .as_deref()
            .map(|path| crate::boot::images::fs::read_full_image(path, provider))
            .transpose(),
        _ => ax_errno::ax_err!(
            InvalidInput,
            "Unsupported image_location; use \"memory\" or enable fs feature for \"fs\""
        ),
    }
}
