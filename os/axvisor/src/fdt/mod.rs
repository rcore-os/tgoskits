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

mod create;
mod device;
mod parser;
mod print;
pub(crate) mod vm_fdt;

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
use ax_lazyinit::LazyInit;
#[cfg(target_arch = "loongarch64")]
use axvm::config::PassThroughDeviceConfig;
// pub use print::print_fdt;
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
))]
pub use create::update_fdt;
pub use device::build_all_node_paths;
use fdt_parser::Fdt;
pub use parser::*;

use axvm::config::AxVMConfig;
use axvmconfig::{AxVMCrateConfig, VMBootProtocol};

use crate::config::{get_vm_dtb_arc, vmcfg};

// DTB cache for generated device trees
static GENERATED_DTB_CACHE: LazyInit<Mutex<BTreeMap<usize, Vec<u8>>>> = LazyInit::new();
#[cfg(target_arch = "loongarch64")]
static LOONGARCH_GUEST_IRQ_ROUTES: LazyInit<Mutex<BTreeMap<usize, Vec<LoongArchGuestIrqRoute>>>> =
    LazyInit::new();

#[cfg(target_arch = "loongarch64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoongArchGuestIrqRoute {
    pub physical_irq: usize,
    pub guest_vector: usize,
}

/// Initialize the DTB cache
pub fn init_dtb_cache() {
    GENERATED_DTB_CACHE.init_once(Mutex::new(BTreeMap::new()));
    #[cfg(target_arch = "loongarch64")]
    LOONGARCH_GUEST_IRQ_ROUTES.init_once(Mutex::new(BTreeMap::new()));
}

/// Get reference to the DTB cache
pub fn dtb_cache() -> &'static Mutex<BTreeMap<usize, Vec<u8>>> {
    GENERATED_DTB_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Generate guest FDT cache the result
/// # Return Value
/// Returns the generated DTB data and stores it in the global cache
pub fn crate_guest_fdt_with_cache(dtb_data: Vec<u8>, crate_config: &AxVMCrateConfig) {
    // Store data in global cache
    let mut cache_lock = dtb_cache().lock();
    cache_lock.insert(crate_config.base.id, dtb_data);
}

#[cfg(target_arch = "loongarch64")]
pub fn store_loongarch_guest_irq_routes(vm_id: usize, routes: Vec<LoongArchGuestIrqRoute>) {
    let mut cache_lock = LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock();
    cache_lock.insert(vm_id, routes);
}

#[cfg(target_arch = "loongarch64")]
pub fn get_loongarch_guest_irq_routes(vm_id: usize) -> Vec<LoongArchGuestIrqRoute> {
    LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .get(&vm_id)
        .cloned()
        .unwrap_or_default()
}

#[cfg(target_arch = "loongarch64")]
fn handle_uefi_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxResult {
    const LOONGARCH_UEFI_FDT_BASE: usize = 0x0010_0000;

    info!(
        "VM[{}] uses LoongArch UEFI boot protocol, keeping firmware DTB at {:#x}",
        vm_config.id(),
        LOONGARCH_UEFI_FDT_BASE
    );
    expand_loongarch_uefi_root_passthrough(vm_config, vm_create_config)?;
    vm_config.set_dtb_load_gpa(LOONGARCH_UEFI_FDT_BASE.into());
    vm_create_config.kernel.dtb_load_addr = Some(LOONGARCH_UEFI_FDT_BASE);
    store_loongarch_guest_irq_routes(vm_config.id(), loongarch_qemu_uefi_irq_routes());
    Ok(())
}

#[cfg(target_arch = "loongarch64")]
fn expand_loongarch_uefi_root_passthrough(
    vm_config: &mut AxVMConfig,
    vm_create_config: &AxVMCrateConfig,
) -> AxResult {
    let has_root_passthrough = vm_config
        .pass_through_devices()
        .iter()
        .any(|device| device.name == "/" && device.length == 0);
    if !has_root_passthrough {
        return Ok(());
    }

    let ranges = ax_driver::probe::acpi::with_acpi(loongarch_acpi_passthrough_ranges)
        .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI root passthrough requires ACPI"))??;

    vm_config.clear_pass_through_devices();
    let mut added = 0;
    for range in ranges
        .into_iter()
        .filter(|range| !loongarch_acpi_passthrough_range_is_occupied(range, vm_create_config))
    {
        vm_config.add_pass_through_device(PassThroughDeviceConfig {
            name: range.name,
            base_gpa: range.base,
            base_hpa: range.base,
            length: range.size,
            irq_id: 0,
        });
        added += 1;
    }

    if added == 0 {
        return Err(ax_err_type!(
            NotFound,
            "LoongArch UEFI root passthrough resources are all occupied by VM config"
        ));
    }

    Ok(())
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_acpi_passthrough_range_is_occupied(
    range: &LoongArchAcpiPassthroughRange,
    vm_create_config: &AxVMCrateConfig,
) -> bool {
    vm_create_config
        .kernel
        .memory_regions
        .iter()
        .any(|memory| loongarch_ranges_overlap(range.base, range.size, memory.gpa, memory.size))
        || vm_create_config.devices.emu_devices.iter().any(|device| {
            loongarch_ranges_overlap(range.base, range.size, device.base_gpa, device.length)
        })
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_ranges_overlap(
    base: usize,
    size: usize,
    other_base: usize,
    other_size: usize,
) -> bool {
    const PAGE_SIZE: usize = 4096;

    if size == 0 || other_size == 0 {
        return false;
    }

    let start = base & !(PAGE_SIZE - 1);
    let end = base.saturating_add(size).div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let other_start = other_base & !(PAGE_SIZE - 1);
    let other_end = other_base.saturating_add(other_size).div_ceil(PAGE_SIZE) * PAGE_SIZE;

    start < other_end && other_start < end
}

#[cfg(target_arch = "loongarch64")]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LoongArchAcpiPassthroughRange {
    name: String,
    base: usize,
    size: usize,
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_acpi_passthrough_ranges(
    acpi: &ax_driver::probe::acpi::System,
) -> AxResult<Vec<LoongArchAcpiPassthroughRange>> {
    let mut ranges = Vec::new();

    for device in acpi.resource_devices().map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("failed to collect ACPI resource devices: {err:?}")
        )
    })? {
        for (index, range) in device.memory_ranges.iter().enumerate() {
            add_loongarch_acpi_passthrough_range(
                &mut ranges,
                format!("{}:mem{index}", device.path),
                range.base,
                range.size,
            )?;
        }
    }

    if let Some(range) = acpi.serial_console_memory_range() {
        add_loongarch_acpi_passthrough_range(
            &mut ranges,
            "acpi-spcr-uart".into(),
            range.base,
            range.size,
        )?;
    }

    for (index, region) in acpi.pci_ecam_regions().iter().enumerate() {
        add_loongarch_acpi_passthrough_range(
            &mut ranges,
            format!("acpi-mcfg{index}"),
            region.base_address,
            region.size() as u64,
        )?;
    }

    for (index, pch_pic) in acpi.routing().pch_pics().iter().enumerate() {
        add_loongarch_acpi_passthrough_range(
            &mut ranges,
            format!("acpi-pch-pic{index}"),
            pch_pic.address,
            u64::from(pch_pic.mmio_size),
        )?;
    }

    for (index, region) in ax_hal::mem::memory_regions()
        .filter(|region| region.flags.contains(ax_hal::mem::MemRegionFlags::DEVICE))
        .enumerate()
    {
        add_loongarch_acpi_passthrough_range(
            &mut ranges,
            format!("host-mmio{index}"),
            region.paddr.as_usize() as u64,
            region.size as u64,
        )?;
    }

    ranges.sort_by_key(|range| range.base);
    let mut merged = Vec::<LoongArchAcpiPassthroughRange>::new();
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            let last_end = last.base.saturating_add(last.size);
            let range_end = range.base.saturating_add(range.size);
            if last_end >= range.base {
                last.size = last_end.max(range_end) - last.base;
                last.name.push('+');
                last.name.push_str(&range.name);
                continue;
            }
        }
        merged.push(range);
    }

    if merged.is_empty() {
        return Err(ax_err_type!(
            NotFound,
            "LoongArch UEFI root passthrough did not find ACPI MMIO resources"
        ));
    }

    Ok(merged)
}

#[cfg(target_arch = "loongarch64")]
fn add_loongarch_acpi_passthrough_range(
    ranges: &mut Vec<LoongArchAcpiPassthroughRange>,
    name: String,
    base: u64,
    size: u64,
) -> AxResult {
    if size == 0 {
        return Ok(());
    }
    let base = usize::try_from(base)
        .map_err(|_| ax_err_type!(InvalidData, "ACPI passthrough base does not fit usize"))?;
    let size = usize::try_from(size)
        .map_err(|_| ax_err_type!(InvalidData, "ACPI passthrough size does not fit usize"))?;
    ranges.push(LoongArchAcpiPassthroughRange { name, base, size });
    Ok(())
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_qemu_uefi_irq_routes() -> Vec<LoongArchGuestIrqRoute> {
    const HOST_UART_IRQ: usize = 2;
    const HOST_PCI_INTX_BASE: usize = 16;
    const GUEST_UART_PCH_INPUT: usize = 2;
    const GUEST_PCH_INTX_BASE: usize = 16;

    let mut routes = Vec::from([LoongArchGuestIrqRoute {
        physical_irq: HOST_UART_IRQ,
        guest_vector: GUEST_UART_PCH_INPUT,
    }]);
    routes.extend((0..4).map(|idx| LoongArchGuestIrqRoute {
        physical_irq: HOST_PCI_INTX_BASE + idx,
        guest_vector: GUEST_PCH_INTX_BASE + idx,
    }));
    routes
}

#[cfg(not(target_arch = "loongarch64"))]
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
    } else {
        #[cfg(target_arch = "loongarch64")]
        {
            warn!(
                "VM[{}] host FDT is unavailable on loongarch64 boot path; skipping \
                 host-FDT-dependent guest DTB handling",
                vm_config.id()
            );
        }

        if let Some(provided_dtb) = get_developer_provided_dtb(vm_config, vm_create_config)? {
            info!("VM[{}] found DTB , parsing...", vm_config.id());
            update_provided_fdt(&provided_dtb, &[], vm_create_config)?;
        } else {
            warn!(
                "VM[{}] no guest DTB provided; continuing without generated DTB",
                vm_config.id()
            );
        }
    }

    // Overlay VM config with the given DTB.
    if let Some(dtb_arc) = get_vm_dtb_arc(vm_config) {
        let dtb = dtb_arc.as_ref();
        parse_reserved_memory_regions(vm_create_config, dtb)?;
        parse_passthrough_devices_address(vm_config, vm_create_config, dtb)?;
        #[cfg(target_arch = "loongarch64")]
        parse_loongarch_guest_irq_routes(vm_config, dtb)?;
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
