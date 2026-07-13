//! RISC-V compatibility facade and target-specific guest FDT policy.

use alloc::vec::Vec;

use ax_errno::AxResult;

use crate::{
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
        decode_interrupt: super::capabilities::decode_plic_source,
    }
}

pub(crate) fn host_fdt_bootarg() -> usize {
    super::capabilities::host_fdt_bootarg()
}

pub(crate) fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    super::capabilities::host_phys_to_virt(paddr)
}

pub(super) fn ensure_chosen_from_host(
    guest_dtb: Vec<u8>,
    host_fdt: Option<&fdt_edit::Fdt>,
) -> AxResult<Vec<u8>> {
    let Some(host_fdt) = host_fdt else {
        return Ok(guest_dtb);
    };
    let mut guest = core::tree::FdtTree::from_bytes(&guest_dtb)?;
    if guest.inner().get_by_path_id("/chosen").is_some() {
        return Ok(guest.finish());
    }
    let Some(host_chosen) = host_fdt.get_by_path_id("/chosen") else {
        return Ok(guest.finish());
    };
    guest.copy_subtree_from(host_fdt, host_chosen, guest.inner().root_id(), false)?;
    Ok(guest.finish())
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxResult<Option<GuestDtbImage>> {
    core::prepare_dtb_guest(vm_config, vm_create_config, provider)
}
