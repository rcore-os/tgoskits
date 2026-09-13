//! RISC-V-specific guest device-tree policy.

use std::vec::Vec;

use crate::{AxVmResult, boot::fdt::core};

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_plic_source,
        resolve_cpu_index: super::capabilities::resolve_cpu_index,
        host_cpu_count: super::capabilities::host_cpu_count,
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
) -> AxVmResult<Vec<u8>> {
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
