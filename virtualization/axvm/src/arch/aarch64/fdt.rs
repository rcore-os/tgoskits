//! AArch64 compatibility facade and target-specific guest FDT policy.

use alloc::vec::Vec;

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

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        describe_aarch64_consoles: true,
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_gic_spi,
        prepare_host_irq_routes: core::forwarded_irq::prepare_aarch64_hybrid_routes,
        enrich_guest_interrupts,
    }
}

fn enrich_guest_interrupts(config: &mut AxVMConfig, dtb: &[u8]) -> AxVmResult {
    if config.interrupt_mode() == axvm_types::VMInterruptMode::Hybrid {
        Ok(())
    } else {
        core::parse_vm_interrupt(config, dtb)
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
    }

    Ok(tree.finish())
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    core::prepare_dtb_guest(vm_config, vm_create_config, provider)
}
