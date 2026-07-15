use alloc::{collections::BTreeMap, format, string::String, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use ax_lazyinit::LazyInit;
use axvmconfig::AxVMCrateConfig;

use super::UEFI_FIRMWARE_FDT_BASE;
use crate::{
    AxVMRef, AxVmResult, ax_err_type,
    config::{AxVMConfig, PassThroughDeviceConfig},
};

static LOONGARCH_GUEST_IRQ_ROUTES: LazyInit<Mutex<BTreeMap<usize, Vec<LoongArchGuestIrqRoute>>>> =
    LazyInit::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoongArchGuestIrqRoute {
    pub physical_irq: usize,
    pub guest_vector: usize,
}

pub fn init() {
    LOONGARCH_GUEST_IRQ_ROUTES.init_once(Mutex::new(BTreeMap::new()));
}

pub fn store_guest_irq_routes(vm_id: usize, routes: Vec<LoongArchGuestIrqRoute>) {
    let mut cache_lock = LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock();
    cache_lock.insert(vm_id, routes);
}

pub fn get_guest_irq_routes(vm_id: usize) -> Vec<LoongArchGuestIrqRoute> {
    LOONGARCH_GUEST_IRQ_ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .get(&vm_id)
        .cloned()
        .unwrap_or_default()
}

pub fn prepare_uefi_fdt_config(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxVmResult {
    info!(
        "VM[{}] uses LoongArch UEFI boot protocol, keeping firmware FDT at {:#x}",
        vm_config.id(),
        UEFI_FIRMWARE_FDT_BASE
    );
    expand_root_passthrough(vm_config, vm_create_config)?;
    vm_config.set_dtb_load_gpa(UEFI_FIRMWARE_FDT_BASE.into());
    vm_create_config.kernel.dtb_load_addr = Some(UEFI_FIRMWARE_FDT_BASE);
    Ok(())
}

pub fn prepare_uefi_runtime_config(vm: &AxVMRef, vm_create_config: &AxVMCrateConfig) {
    store_guest_irq_routes(vm.id(), super::guest_irq_routes(vm, vm_create_config));
}

fn expand_root_passthrough(
    vm_config: &mut AxVMConfig,
    vm_create_config: &AxVMCrateConfig,
) -> AxVmResult {
    let has_root_passthrough = vm_config
        .pass_through_devices()
        .iter()
        .any(|device| device.name == "/" && device.length == 0);
    if !has_root_passthrough {
        return Ok(());
    }

    let ranges = ax_driver::probe::acpi::with_acpi(acpi_passthrough_ranges)
        .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI root passthrough requires ACPI"))??;

    vm_config.clear_pass_through_devices();
    let mut added = 0;
    for range in ranges
        .into_iter()
        .filter(|range| !passthrough_range_is_occupied(range, vm_create_config))
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

fn passthrough_range_is_occupied(
    range: &AcpiPassthroughRange,
    vm_create_config: &AxVMCrateConfig,
) -> bool {
    vm_create_config
        .kernel
        .memory_regions
        .iter()
        .any(|memory| ranges_overlap(range.base, range.size, memory.gpa, memory.size))
        || vm_create_config
            .devices
            .emu_devices
            .iter()
            .any(|device| ranges_overlap(range.base, range.size, device.base_gpa, device.length))
}

fn ranges_overlap(base: usize, size: usize, other_base: usize, other_size: usize) -> bool {
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AcpiPassthroughRange {
    name: String,
    base: usize,
    size: usize,
}

fn acpi_passthrough_ranges(
    acpi: &ax_driver::probe::acpi::System,
) -> AxVmResult<Vec<AcpiPassthroughRange>> {
    let mut ranges = Vec::new();

    for device in acpi.resource_devices().map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("failed to collect ACPI resource devices: {err:?}")
        )
    })? {
        for (index, range) in device.memory_ranges.iter().enumerate() {
            add_acpi_passthrough_range(
                &mut ranges,
                format!("{}:mem{index}", device.path),
                range.base,
                range.size,
            )?;
        }
    }

    if let Some(range) = acpi.serial_console_memory_range() {
        add_acpi_passthrough_range(&mut ranges, "acpi-spcr-uart".into(), range.base, range.size)?;
    }

    for (index, region) in acpi.pci_ecam_regions().iter().enumerate() {
        add_acpi_passthrough_range(
            &mut ranges,
            format!("acpi-mcfg{index}"),
            region.base_address,
            region.size() as u64,
        )?;
    }

    for (index, pch_pic) in acpi.routing().pch_pics().iter().enumerate() {
        add_acpi_passthrough_range(
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
        add_acpi_passthrough_range(
            &mut ranges,
            format!("host-mmio{index}"),
            region.paddr.as_usize() as u64,
            region.size as u64,
        )?;
    }

    ranges.sort_by_key(|range| range.base);
    let mut merged = Vec::<AcpiPassthroughRange>::new();
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

fn add_acpi_passthrough_range(
    ranges: &mut Vec<AcpiPassthroughRange>,
    name: String,
    base: u64,
    size: u64,
) -> AxVmResult {
    if size == 0 {
        return Ok(());
    }
    let base = usize::try_from(base)
        .map_err(|_| ax_err_type!(InvalidData, "ACPI passthrough base does not fit usize"))?;
    let size = usize::try_from(size)
        .map_err(|_| ax_err_type!(InvalidData, "ACPI passthrough size does not fit usize"))?;
    ranges.push(AcpiPassthroughRange { name, base, size });
    Ok(())
}
