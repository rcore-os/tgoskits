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

use alloc::vec::Vec;
use core::ptr::NonNull;

use ax_memory_addr::MemoryAddr;
use axvmconfig::AxVMCrateConfig;

use super::tree::{FdtTree, GuestMemorySpec};
use crate::{
    AxVMRef, AxVmResult, GuestPhysAddr, VMMemoryRegion, ax_err_type,
    boot::images::load_vm_image_from_memory,
};

fn guest_memory_specs(
    new_memory: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
) -> Vec<GuestMemorySpec> {
    let configured_region_count = crate_config
        .memory
        .regions
        .iter()
        .filter(|region| !matches!(region.backing, axvmconfig::MemoryBackingConfig::Reserved))
        .count();

    if new_memory.len() < configured_region_count {
        warn!(
            "VM memory region count {} is smaller than configured guest RAM count {}; filtering \
             /memory by runtime order",
            new_memory.len(),
            configured_region_count
        );
    }

    new_memory
        .iter()
        .take(configured_region_count)
        .map(|mem| GuestMemorySpec::new(mem.gpa.as_usize() as u64, mem.size() as u64))
        .collect()
}

#[cfg(test)]
fn initrd_range_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    let start = ramdisk.load_gpa.as_usize() as u64;
    let size = ramdisk.size? as u64;
    Some((start, start.saturating_add(size)))
}

pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxVmResult {
    let patch_runtime = super::selected_guest_fdt_policy().patch_runtime;
    // SAFETY: `fdt_src` originates from `GuestDtbImage::as_bytes`, and the
    // caller supplies the exact slice length while the image remains borrowed.
    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let new_fdt_bytes = patch_runtime(fdt_bytes, &vm, crate_config)?;

    load_patched_fdt(vm, new_fdt_bytes)
}

fn load_patched_fdt(vm: AxVMRef, new_fdt_bytes: Vec<u8>) -> AxVmResult {
    let dest_addr = calculate_dtb_load_addr(vm.clone(), new_fdt_bytes.len())?;
    debug!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm.clone())?;
    vm.set_guest_device_tree(dest_addr, new_fdt_bytes)
}

pub fn patch_guest_fdt_for_runtime(
    fdt_bytes: &[u8],
    memory_regions: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
    initrd_start_size: Option<(u64, u64)>,
    create_chosen: bool,
) -> AxVmResult<Vec<u8>> {
    let mut tree = FdtTree::from_bytes(fdt_bytes)?;
    let memory_specs = guest_memory_specs(memory_regions, crate_config);
    tree.rebuild_memory_nodes(&memory_specs)?;
    if create_chosen
        || initrd_start_size.is_some()
        || tree.inner().get_by_path_id("/chosen").is_some()
    {
        tree.patch_chosen(initrd_start_size)?;
    }
    Ok(tree.finish())
}

pub(crate) fn calculate_dtb_load_addr(vm: AxVMRef, fdt_size: usize) -> AxVmResult<GuestPhysAddr> {
    const MB: usize = 1024 * 1024;

    let main_memory =
        vm.memory_regions().first().cloned().ok_or_else(|| {
            ax_err_type!(InvalidInput, "VM has no memory region for DTB placement")
        })?;

    let dtb_addr = vm.with_config_mut(|config| -> AxVmResult<GuestPhysAddr> {
        let use_configured_dtb_addr =
            config.image_config.dtb_load_gpa.is_some() && !main_memory.is_identical();

        let dtb_addr = if let Some(configured) = config
            .image_config
            .dtb_load_gpa
            .filter(|_| use_configured_dtb_addr)
        {
            configured
        } else {
            let main_memory_size = main_memory.size().min(512 * MB);
            default_dtb_load_addr(main_memory.gpa, main_memory_size, fdt_size)?
        };
        config.image_config.dtb_load_gpa = Some(dtb_addr);
        Ok(dtb_addr)
    })?;

    Ok(dtb_addr)
}

fn default_dtb_load_addr(
    memory_base: GuestPhysAddr,
    memory_size: usize,
    fdt_size: usize,
) -> AxVmResult<GuestPhysAddr> {
    const DTB_ALIGNMENT: usize = 2 * 1024 * 1024;

    if fdt_size == 0 || fdt_size > memory_size {
        return Err(ax_err_type!(
            InvalidInput,
            alloc::format!(
                "DTB size {fdt_size:#x} does not fit placement window of {memory_size:#x} bytes"
            )
        ));
    }
    let memory_end = memory_base
        .as_usize()
        .checked_add(memory_size)
        .ok_or_else(|| ax_err_type!(InvalidInput, "DTB placement window address overflows"))?;
    let unaligned = memory_end
        .checked_sub(fdt_size)
        .ok_or_else(|| ax_err_type!(InvalidInput, "DTB placement address underflows"))?;
    let aligned = GuestPhysAddr::from(unaligned).align_down(DTB_ALIGNMENT);
    if aligned < memory_base {
        return Err(ax_err_type!(
            InvalidInput,
            "DTB cannot satisfy 2 MiB alignment inside the guest memory window"
        ));
    }
    Ok(aligned)
}

#[cfg(test)]
mod tests {
    use axvmconfig::AxVMCrateConfig;
    use fdt_edit::Fdt;

    use super::{
        super::tree::sanitize_bootargs, default_dtb_load_addr, initrd_range_from_image_config,
    };
    use crate::{GuestPhysAddr, config::RamdiskInfo};

    #[test]
    fn initrd_range_requires_both_address_and_size() {
        assert_eq!(
            initrd_range_from_image_config(Some(&RamdiskInfo {
                load_gpa: GuestPhysAddr::from(0xa000_0000usize),
                size: None,
            })),
            None
        );
        assert_eq!(
            initrd_range_from_image_config(Some(&RamdiskInfo {
                load_gpa: GuestPhysAddr::from(0xa000_0000usize),
                size: Some(0x1234),
            })),
            Some((0xa000_0000, 0xa000_1234))
        );
    }

    #[test]
    fn oversized_dtb_is_rejected_without_address_underflow() {
        assert!(
            default_dtb_load_addr(GuestPhysAddr::from(0x8000_0000), 0x20_0000, 0x20_0001).is_err()
        );
    }

    #[test]
    fn default_dtb_address_remains_inside_the_placement_window() {
        let address =
            default_dtb_load_addr(GuestPhysAddr::from(0x8000_0000), 0x1000_0000, 0x12_345).unwrap();

        assert_eq!(address, GuestPhysAddr::from(0x8fe0_0000));
    }

    #[test]
    fn sanitize_bootargs_enables_auto_repair_for_block_roots() {
        let bootargs = "root=/dev/mmcblk0p2 rw console=ttyS2,1500000 rootwait rootfstype=ext4";

        assert_eq!(
            sanitize_bootargs(bootargs),
            "root=/dev/mmcblk0p2 rw console=ttyS2,1500000 rootwait rootfstype=ext4 fsck.repair=yes"
        );
    }

    #[test]
    fn sanitize_bootargs_preserves_existing_fsck_policy() {
        let bootargs =
            "root=/dev/mmcblk0p2 ro rootwait rootfstype=ext4 fsckfix rdinit=/init root=/dev/ram0";

        assert_eq!(
            sanitize_bootargs(bootargs),
            "root=/dev/mmcblk0p2 rw rootwait rootfstype=ext4 fsckfix"
        );
    }

    #[test]
    fn runtime_patch_only_creates_chosen_when_requested() {
        let fdt = Fdt::new();
        let dtb = fdt.encode().as_ref().to_vec();
        let cfg = AxVMCrateConfig::default();

        let patched = super::patch_guest_fdt_for_runtime(&dtb, &[], &cfg, None, false).unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_none());

        let patched = super::patch_guest_fdt_for_runtime(&dtb, &[], &cfg, None, true).unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_some());
    }
}
