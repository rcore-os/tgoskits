//! Architecture-neutral guest device-tree preparation.

use std::{format, vec::Vec};

use axvmconfig::{GuestConfig, VMBootProtocol};

use crate::{
    AxVmResult, ax_err, ax_err_type,
    boot::{BootImageProvider, fdt::GuestDtbImage},
    config::AxVMConfig,
};

pub(crate) mod create;
mod device;
pub(crate) mod interrupt;
mod parser;
pub(crate) mod pci;
mod policy;
mod print;
pub(crate) mod serial;
pub(crate) mod timer;
pub(crate) mod tree;

#[cfg(test)]
mod tree_tests;

#[cfg(test)]
pub use create::update_fdt;
pub use parser::*;
pub use policy::{DecodedInterrupt, GuestFdtPolicy};

pub fn prepare_dtb_guest(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    let host_fdt_bytes = try_get_host_fdt();
    resolve_machine_resources_from_host(vm_config, host_fdt_bytes)?;

    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        skip_guest_dtb(vm_config, vm_create_config);
        return Ok(None);
    }

    let guest_dtb = build_guest_dtb(vm_config, vm_create_config, provider, host_fdt_bytes)?;
    enrich_guest_config(vm_config, vm_create_config, guest_dtb.as_ref())?;
    Ok(guest_dtb)
}

fn resolve_machine_resources_from_host(
    vm_config: &mut AxVMConfig,
    host_fdt_bytes: Option<&[u8]>,
) -> AxVmResult {
    let Some(host_fdt_bytes) = host_fdt_bytes else {
        return Ok(());
    };
    let host_fdt = fdt_edit::Fdt::from_bytes(host_fdt_bytes).map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse host FDT while resolving the virtual UART: {err:#?}")
        )
    })?;
    let machine = crate::machine::current_machine_profile(vm_config.phys_cpu_ls.cpu_num());
    let current = vm_config.serial_profile();
    if let Some(interrupt_encoding) = machine.serial_fdt_interrupt
        && let Some(resolved) =
            serial::host_selected_serial(&host_fdt, current, interrupt_encoding)?
    {
        if resolved.profile != current {
            info!(
                "VM[{}] virtual UART follows the host-selected UART: {:?}",
                vm_config.id(),
                resolved.profile
            );
        }
        vm_config.replace_machine_serial(resolved.profile, Some(resolved.identity))?;
    }

    if let Some(gic) = interrupt::host_gic_profile(&host_fdt)? {
        info!(
            "VM[{}] virtual GIC follows host firmware resources: {:?}",
            vm_config.id(),
            gic
        );
        vm_config.replace_machine_gic(gic)?;
    }
    if machine.timer.is_some() {
        let timer = timer::host_timer_profile(&host_fdt)?.ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "host FDT does not provide a valid arm,armv8-timer node"
            )
        })?;
        info!(
            "VM[{}] architectural timer follows host firmware PPIs",
            vm_config.id()
        );
        vm_config.replace_machine_timer(timer)?;
    }
    if let Some(plic) = interrupt::host_plic_profile(&host_fdt)? {
        info!(
            "VM[{}] virtual PLIC follows host firmware resources: {:?}",
            vm_config.id(),
            plic
        );
        vm_config.replace_machine_plic(plic)?;
    }
    Ok(())
}

pub(crate) fn selected_guest_fdt_policy() -> GuestFdtPolicy {
    super::guest_fdt_policy()
}

fn skip_guest_dtb(vm_config: &mut AxVMConfig, vm_create_config: &mut GuestConfig) {
    info!(
        "VM[{}] uses UEFI boot protocol, skipping guest DTB handling",
        vm_config.id()
    );
    vm_config.clear_dtb_load_gpa();
    vm_create_config.kernel.dtb_load_addr = None;
}

fn build_guest_dtb(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    provider: &dyn BootImageProvider,
    host_fdt_bytes: Option<&'static [u8]>,
) -> AxVmResult<Option<GuestDtbImage>> {
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

fn parse_host_fdt(host_fdt_bytes: &'static [u8]) -> AxVmResult<fdt_edit::Fdt> {
    fdt_edit::Fdt::from_bytes(host_fdt_bytes)
        .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {err:#?}")))
}

fn enrich_guest_config(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut GuestConfig,
    guest_dtb: Option<&GuestDtbImage>,
) -> AxVmResult {
    let Some(dtb) = guest_dtb.map(GuestDtbImage::as_bytes) else {
        clear_unresolved_dtb_config(vm_config, vm_create_config);
        return Ok(());
    };

    parse_reserved_memory_regions(vm_create_config, dtb)?;
    parse_passthrough_devices_address(vm_config, vm_create_config, dtb)?;
    parse_vm_interrupt(vm_config, vm_create_config, dtb)
}

fn clear_unresolved_dtb_config(vm_config: &mut AxVMConfig, vm_create_config: &mut GuestConfig) {
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
    crate_config: &GuestConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<Vec<u8>>> {
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
        _ => ax_err!(
            InvalidInput,
            "Unsupported image_location; use \"memory\" or enable fs feature for \"fs\""
        ),
    }
}
