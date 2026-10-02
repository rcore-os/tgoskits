//! AArch64-specific guest device-tree policy.

use std::vec::Vec;

use fdt_edit::Fdt;

use crate::{AxVmResult, boot::fdt::core};

pub(crate) fn host_gic_maintenance_intid(fdt: &Fdt) -> AxVmResult<Option<u32>> {
    core::interrupt::host_gic_maintenance_intid(fdt)
}

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_gic_spi,
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

pub(super) fn initrd_start_size_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    Some((ramdisk.load_gpa.as_usize() as u64, ramdisk.size? as u64))
}

pub(super) fn update_cpu_node(
    fdt: &Fdt,
    host_fdt: Option<&Fdt>,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    match host_fdt {
        Some(host) => core::cpu::project_cpus(fdt, host, crate_config),
        None => Ok(fdt.encode().as_ref().to_vec()),
    }
}
