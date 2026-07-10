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

#[cfg(target_arch = "riscv64")]
use alloc::format;
use alloc::{string::String, vec::Vec};
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use core::ptr::NonNull;

use ax_errno::{AxResult, ax_err_type};
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_memory_addr::MemoryAddr;
use axvmconfig::AxVMCrateConfig;
use fdt_edit::{Fdt, Node, NodeId};

use super::tree::FdtTree;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
use super::tree::GuestMemorySpec;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
use crate::VMMemoryRegion;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::{AxVMRef, GuestPhysAddr, boot::images::load_vm_image_from_memory};

pub fn create_guest_fdt(
    fdt: &Fdt,
    passthrough_device_names: &[String],
    crate_config: &AxVMCrateConfig,
) -> AxResult<Vec<u8>> {
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;

    let guest_tree = FdtTree::clone_filtered(fdt, |node_id, path, node| {
        should_keep_generated_node(
            fdt,
            node_id,
            path,
            node,
            passthrough_device_names,
            phys_cpu_ids,
        )
    })?;
    Ok(guest_tree.finish())
}

fn should_keep_generated_node(
    fdt: &Fdt,
    node_id: NodeId,
    node_path: &str,
    node: &Node,
    passthrough_device_names: &[String],
    phys_cpu_ids: &[usize],
) -> bool {
    if node.name().starts_with("memory") {
        return false;
    }

    if node_path == "/cpus" || node_path.starts_with("/cpus/cpu-map") {
        return true;
    }

    if node_path.starts_with("/cpus/cpu@") {
        return need_cpu_node(phys_cpu_ids, fdt, node_id, node_path);
    }

    passthrough_device_names
        .iter()
        .any(|device_path| device_path == node_path)
        || is_descendant_of_passthrough_device(node_path, passthrough_device_names)
        || is_ancestor_of_passthrough_device(node_path, passthrough_device_names)
}

fn is_descendant_of_passthrough_device(
    node_path: &str,
    passthrough_device_names: &[String],
) -> bool {
    passthrough_device_names.iter().any(|passthrough_path| {
        node_path
            .strip_prefix(passthrough_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn is_ancestor_of_passthrough_device(node_path: &str, passthrough_device_names: &[String]) -> bool {
    passthrough_device_names.iter().any(|passthrough_path| {
        passthrough_path
            .strip_prefix(node_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
            || node_path == "/"
    })
}

fn cpu_node_id(node_path: &str) -> Option<usize> {
    node_path
        .strip_prefix("/cpus/cpu@")
        .and_then(|rest| rest.split('/').next())
        .and_then(|id| usize::from_str_radix(id, 16).ok())
}

fn cpu_reg_address(fdt: &Fdt, node_id: NodeId) -> Option<usize> {
    fdt.view_typed(node_id)
        .and_then(|node| node.regs().first().map(|reg| reg.address as usize))
}

fn need_cpu_node(phys_cpu_ids: &[usize], fdt: &Fdt, node_id: NodeId, node_path: &str) -> bool {
    if !node_path.starts_with("/cpus/cpu@") {
        return true;
    }

    if let Some(cpu_id) = cpu_node_id(node_path) {
        return phys_cpu_ids.contains(&cpu_id);
    }

    cpu_reg_address(fdt, node_id).is_some_and(|cpu_address| {
        debug!("Checking CPU node {node_path} with address 0x{cpu_address:x}");
        phys_cpu_ids.contains(&cpu_address)
    })
}

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
fn guest_memory_specs(
    new_memory: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
) -> Vec<GuestMemorySpec> {
    let configured_region_count = if crate_config.kernel.configured_memory_region_count == 0 {
        crate_config.kernel.memory_regions.len()
    } else {
        crate_config
            .kernel
            .configured_memory_region_count
            .min(crate_config.kernel.memory_regions.len())
    };

    if new_memory.len() != crate_config.kernel.memory_regions.len() {
        warn!(
            "VM memory region count {} does not match config region count {}; filtering /memory \
             by zipped order",
            new_memory.len(),
            crate_config.kernel.memory_regions.len()
        );
    }

    new_memory
        .iter()
        .take(configured_region_count)
        .zip(
            crate_config
                .kernel
                .memory_regions
                .iter()
                .take(configured_region_count),
        )
        .map(|(mem, _cfg)| GuestMemorySpec::new(mem.gpa.as_usize() as u64, mem.size() as u64))
        .collect()
}

#[cfg(any(test, target_arch = "aarch64"))]
fn initrd_start_size_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let rd = ramdisk?;
    let start = rd.load_gpa.as_usize() as u64;
    let size = rd.size? as u64;
    Some((start, size))
}

#[cfg(test)]
fn initrd_range_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let (start, size) = initrd_start_size_from_image_config(ramdisk)?;
    Some((start, start.saturating_add(size)))
}

#[cfg(target_arch = "aarch64")]
pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxResult {
    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let initrd_start_size = vm.with_config(|config| {
        initrd_start_size_from_image_config(config.image_config.ramdisk.as_ref())
    });
    let new_fdt_bytes = patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        crate_config,
        initrd_start_size,
        true,
    )?;

    load_patched_fdt(vm, new_fdt_bytes)
}

#[cfg(target_arch = "riscv64")]
pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxResult {
    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let host_fdt = super::try_get_host_fdt()
        .map(Fdt::from_bytes)
        .transpose()
        .map_err(|e| {
            ax_err_type!(
                InvalidData,
                format!("Failed to parse host FDT while updating guest FDT: {e:#?}")
            )
        })?;
    let new_fdt_bytes =
        patch_guest_fdt_for_runtime(fdt_bytes, &vm.memory_regions(), crate_config, None, false)?;
    let new_fdt_bytes = ensure_chosen_from_host(new_fdt_bytes, host_fdt.as_ref())?;

    load_patched_fdt(vm, new_fdt_bytes)
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn load_patched_fdt(vm: AxVMRef, new_fdt_bytes: Vec<u8>) -> AxResult {
    let dest_addr = calculate_dtb_load_addr(vm.clone(), new_fdt_bytes.len())?;
    debug!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm.clone())?;
    vm.set_guest_device_tree(dest_addr, new_fdt_bytes)
}

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn patch_guest_fdt_for_runtime(
    fdt_bytes: &[u8],
    memory_regions: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
    initrd_start_size: Option<(u64, u64)>,
    create_chosen: bool,
) -> AxResult<Vec<u8>> {
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

#[cfg(target_arch = "riscv64")]
fn ensure_chosen_from_host(guest_dtb: Vec<u8>, host_fdt: Option<&Fdt>) -> AxResult<Vec<u8>> {
    let Some(host_fdt) = host_fdt else {
        return Ok(guest_dtb);
    };
    let mut guest = FdtTree::from_bytes(&guest_dtb)?;
    if guest.inner().get_by_path_id("/chosen").is_some() {
        return Ok(guest.finish());
    }
    let Some(host_chosen) = host_fdt.get_by_path_id("/chosen") else {
        return Ok(guest.finish());
    };
    guest.copy_subtree_from(host_fdt, host_chosen, guest.inner().root_id(), false)?;
    Ok(guest.finish())
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn calculate_dtb_load_addr(vm: AxVMRef, fdt_size: usize) -> AxResult<GuestPhysAddr> {
    const MB: usize = 1024 * 1024;

    let main_memory =
        vm.memory_regions().first().cloned().ok_or_else(|| {
            ax_err_type!(InvalidInput, "VM has no memory region for DTB placement")
        })?;

    let dtb_addr = vm.with_config(|config| {
        let use_configured_dtb_addr =
            config.image_config.dtb_load_gpa.is_some() && !main_memory.is_identical();

        let dtb_addr = if use_configured_dtb_addr {
            config.image_config.dtb_load_gpa.unwrap()
        } else {
            let main_memory_size = main_memory.size().min(512 * MB);
            let addr = (main_memory.gpa + main_memory_size - fdt_size).align_down(2 * MB);
            if fdt_size > main_memory_size {
                error!("DTB size is larger than available memory");
            }
            addr
        };
        config.image_config.dtb_load_gpa = Some(dtb_addr);
        dtb_addr
    });

    Ok(dtb_addr)
}

#[cfg(target_arch = "aarch64")]
pub fn update_cpu_node(
    fdt: &Fdt,
    host_fdt: Option<&Fdt>,
    crate_config: &AxVMCrateConfig,
) -> AxResult<Vec<u8>> {
    let Some(host_fdt) = host_fdt else {
        return Ok(fdt.encode().as_ref().to_vec());
    };

    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;
    let mut tree = FdtTree::from_fdt(fdt.clone());
    tree.inner_mut().remove_by_path("/cpus");

    if let Some(host_cpus_id) = host_fdt.get_by_path_id("/cpus") {
        let cpus_id =
            tree.copy_subtree_from(host_fdt, host_cpus_id, tree.inner().root_id(), true)?;
        let cpu_paths = tree
            .node_paths()
            .into_iter()
            .filter_map(|(id, path)| {
                (path.starts_with("/cpus/cpu@")
                    && !need_cpu_node(phys_cpu_ids, tree.inner(), id, &path))
                .then_some(path)
            })
            .collect::<Vec<_>>();
        for path in cpu_paths {
            tree.inner_mut().remove_by_path(&path);
        }
        if let Some(cpus) = tree.inner_mut().node_mut(cpus_id) {
            for prop in [
                "riscv,cbop-block-size",
                "riscv,cboz-block-size",
                "riscv,cbom-block-size",
            ] {
                cpus.remove_property(prop);
            }
        }
    }

    Ok(tree.finish())
}

#[cfg(test)]
mod tests {
    use axvmconfig::AxVMCrateConfig;
    use fdt_edit::{Fdt, Node, Property};
    use fdt_raw::RegInfo;

    use super::{
        super::tree::sanitize_bootargs, cpu_node_id, initrd_range_from_image_config, need_cpu_node,
    };
    use crate::{GuestPhysAddr, config::RamdiskInfo};

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut prop = Property::new(name, alloc::vec![]);
        prop.set_u32_ls(&[value]);
        prop
    }

    fn test_fdt(dts: &str) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let cpus = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#size-cells", 0));

        for line in dts.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (name, reg) = line.split_once('=').unwrap();
            let node = fdt.add_node(cpus, Node::new(name));
            let reg = usize::from_str_radix(reg, 16).unwrap();
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(reg as u64, None)]);
        }

        fdt
    }

    #[test]
    fn cpu_node_selection_uses_node_id_when_reg_differs() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let selected: alloc::vec::Vec<_> = fdt
            .iter_node_ids()
            .map(|id| (id, fdt.path_of(id)))
            .filter(|(_, path)| path.starts_with("/cpus/cpu@"))
            .filter_map(|(id, path)| need_cpu_node(&[0x100], &fdt, id, &path).then_some(path))
            .collect();

        assert_eq!(selected, ["/cpus/cpu@100"]);
    }

    #[test]
    fn cpu_node_id_parses_hex_unit_address() {
        assert_eq!(cpu_node_id("/cpus/cpu@100"), Some(0x100));
    }

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
    fn runtime_patch_can_leave_missing_chosen_for_host_copy() {
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

    #[test]
    fn generated_fdt_filters_cpu_nodes_by_unit_address() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let cfg = AxVMCrateConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(alloc::vec![0x100]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(reparsed.get_by_path_id("/cpus/cpu@100").is_some());
        assert!(reparsed.get_by_path_id("/cpus/cpu@0").is_none());
        assert!(reparsed.get_by_path_id("/cpus/cpu@101").is_none());
    }
}
