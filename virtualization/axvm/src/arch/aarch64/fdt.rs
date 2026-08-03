//! AArch64 compatibility facade and target-specific guest FDT policy.

use alloc::{format, vec::Vec};

use fdt_edit::Fdt;

use crate::{
    AxVmResult, ax_err_type,
    boot::{BootImageProvider, fdt::GuestDtbImage},
    config::AxVMConfig,
};

#[path = "../../boot/fdt/core/mod.rs"]
pub(crate) mod core;

pub use core::{
    parse_passthrough_devices_address, parse_reserved_memory_regions, parse_vm_interrupt,
    reserve_excluded_device_ranges, set_phys_cpu_sets, setup_guest_fdt_from_vmm, try_get_host_fdt,
    update_fdt, update_provided_fdt,
};

const ARCH_TIMER_COMPATIBLE: &str = "arm,armv8-timer";
const ARCH_TIMER_VIRTUAL_IRQ_INDEX: usize = 2;
const GIC_PPI_TYPE: u32 = 1;
const GIC_PPI_BASE: u32 = 16;
const GIC_PPI_COUNT: u32 = 16;

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_gic_spi,
    }
}

pub(crate) fn host_fdt_bootarg() -> usize {
    super::capabilities::host_fdt_bootarg()
}

pub(crate) fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    super::capabilities::host_phys_to_virt(paddr)
}

pub(super) fn initrd_start_size_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    Some((ramdisk.load_gpa.as_usize() as u64, ramdisk.size? as u64))
}

pub(super) fn update_cpu_node(
    fdt: &Fdt,
    host_fdt: Option<&Fdt>,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    let Some(host_fdt) = host_fdt else {
        return Ok(fdt.encode().as_ref().to_vec());
    };

    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;
    let mut tree = core::tree::FdtTree::from_fdt(fdt.clone());
    tree.inner_mut().remove_by_path("/cpus");

    if let Some(host_cpus_id) = host_fdt.get_by_path_id("/cpus") {
        let cpus_id =
            tree.copy_subtree_from(host_fdt, host_cpus_id, tree.inner().root_id(), true)?;
        let cpu_paths = tree
            .node_paths()
            .into_iter()
            .filter_map(|(id, path)| {
                (path.starts_with("/cpus/cpu@")
                    && !core::create::need_cpu_node(phys_cpu_ids, tree.inner(), id, &path))
                .then_some(path)
            })
            .collect::<Vec<_>>();
        for path in cpu_paths {
            tree.inner_mut().remove_by_path(&path);
        }
        if let Some(cpus) = tree.inner_mut().node_mut(cpus_id) {
            for property in [
                "riscv,cbop-block-size",
                "riscv,cboz-block-size",
                "riscv,cbom-block-size",
            ] {
                cpus.remove_property(property);
            }
        }

        // AxVisor does not currently implement PSCI CPU_SUSPEND. Do not copy
        // host idle states that would make a guest issue unsupported calls on
        // every idle transition.
        tree.remove_cpu_idle_states();
    }

    Ok(tree.finish())
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    let configured_timer_irq = vm_config
        .aarch64_virtual_timer_irq()
        .map(validate_gic_ppi_irq)
        .transpose()?;
    let guest_dtb = core::prepare_dtb_guest(vm_config, vm_create_config, provider)?;
    let timer_irq = guest_dtb
        .as_ref()
        .map(|dtb| aarch64_virtual_timer_irq_from_fdt(dtb.as_bytes()))
        .transpose()?
        .flatten();
    vm_config.set_aarch64_virtual_timer_irq(timer_irq.or(configured_timer_irq));
    Ok(guest_dtb)
}

pub(super) fn aarch64_virtual_timer_irq_from_fdt(dtb: &[u8]) -> AxVmResult<Option<u32>> {
    let fdt = Fdt::from_bytes(dtb).map_err(|error| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse AArch64 timer route from FDT: {error:#?}")
        )
    })?;

    for timer in fdt.find_compatible(&[ARCH_TIMER_COMPATIBLE]) {
        if timer
            .as_node()
            .get_property("status")
            .and_then(|status| status.as_str())
            == Some("disabled")
        {
            continue;
        }

        let interrupts = timer.interrupts();
        let timer_interrupt = interrupts
            .get(ARCH_TIMER_VIRTUAL_IRQ_INDEX)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!(
                        "AArch64 timer node {} has no virtual-timer interrupt",
                        timer.path()
                    )
                )
            })?;
        return decode_gic_ppi(&timer_interrupt.specifier).map(Some);
    }

    Ok(None)
}

fn decode_gic_ppi(specifier: &[u32]) -> AxVmResult<u32> {
    let (&interrupt_type, &ppi_offset) =
        specifier.first().zip(specifier.get(1)).ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "Arm timer IRQ specifier has fewer than 2 cells"
            )
        })?;
    if interrupt_type != GIC_PPI_TYPE {
        return Err(ax_err_type!(
            InvalidData,
            format!("Arm virtual timer IRQ type {interrupt_type} is not a GIC PPI")
        ));
    }
    if ppi_offset >= GIC_PPI_COUNT {
        return Err(ax_err_type!(
            InvalidData,
            format!("Arm virtual timer PPI offset {ppi_offset} is out of range")
        ));
    }
    validate_gic_ppi_irq(GIC_PPI_BASE + ppi_offset)
}

fn validate_gic_ppi_irq(irq: u32) -> AxVmResult<u32> {
    if !(GIC_PPI_BASE..GIC_PPI_BASE + GIC_PPI_COUNT).contains(&irq) {
        return Err(ax_err_type!(
            InvalidInput,
            format!("Arm virtual timer IRQ {irq} is not a GIC PPI")
        ));
    }
    Ok(irq)
}
