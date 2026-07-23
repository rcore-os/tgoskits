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
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_gic_spi,
        normalize_host_derived: retain_host_derived_controller_properties,
    }
}

fn retain_host_derived_controller_properties(
    _host_fdt: &Fdt,
    _guest_tree: &mut core::tree::FdtTree,
) -> AxVmResult {
    Ok(())
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
    if let Some(cpus_id) =
        core::create::replace_cpu_subtree_from_host(&mut tree, host_fdt, phys_cpu_ids)?
    {
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
