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

//! Architecture-neutral FDT parsing and guest configuration enrichment.

use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use axvmconfig::{
    AxVMCrateConfig, PassThroughDeviceConfig, ReservedAddressConfig, VmMemConfig, VmMemMappingType,
};
use fdt_edit::{Fdt, Node, NodeType, PciRange, PciSpace};

use crate::{AxVmResult, MappingFlags, ax_err_type, config::AxVMConfig};

const PAGE_SIZE_4K: usize = 0x1000;

pub fn try_get_host_fdt() -> Option<&'static [u8]> {
    let bootarg = super::super::host_fdt_bootarg();
    if bootarg == 0 {
        warn!("Boot argument does not contain a host FDT pointer");
        return None;
    }

    let fdt_vaddr = super::super::host_phys_to_virt(bootarg.into());
    super::tree::host_fdt_bytes_from_ptr(fdt_vaddr.as_ptr()).inspect(|bytes| {
        trace!("Host FDT size: 0x{:x}", bytes.len());
    })
}

pub fn setup_guest_fdt_from_vmm(
    fdt_bytes: &[u8],
    vm_cfg: &mut AxVMConfig,
    crate_config: &AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {e:#?}")))?;

    reserve_excluded_device_ranges(vm_cfg, crate_config, fdt_bytes)?;
    let passthrough_device_names = super::device::find_all_passthrough_devices(vm_cfg, &fdt);
    super::create::create_guest_fdt(&fdt, &passthrough_device_names, crate_config)
}

fn is_reserved_memory_path(node_path: &str) -> bool {
    node_path == "/reserved-memory" || node_path.starts_with("/reserved-memory/")
}

fn overlaps_memory_region(lhs_gpa: usize, lhs_size: usize, rhs: &VmMemConfig) -> bool {
    let lhs_end = lhs_gpa.saturating_add(lhs_size);
    let rhs_end = rhs.gpa.saturating_add(rhs.size);
    lhs_gpa < rhs_end && rhs.gpa < lhs_end
}

fn align_down_4k(value: usize) -> usize {
    value & !(PAGE_SIZE_4K - 1)
}

fn align_up_4k(value: usize) -> usize {
    value
        .saturating_add(PAGE_SIZE_4K - 1)
        .checked_div(PAGE_SIZE_4K)
        .unwrap_or(usize::MAX / PAGE_SIZE_4K)
        .saturating_mul(PAGE_SIZE_4K)
}

fn align_reserved_region_4k(gpa: usize, size: usize) -> Option<(usize, usize)> {
    if size == 0 {
        return None;
    }

    let aligned_gpa = align_down_4k(gpa);
    let end = gpa.saturating_add(size);
    let aligned_end = align_up_4k(end);
    let aligned_size = aligned_end.saturating_sub(aligned_gpa);

    (aligned_size > 0).then_some((aligned_gpa, aligned_size))
}

fn subtract_memory_region_overlap(
    start: usize,
    size: usize,
    existing_regions: &[VmMemConfig],
) -> Vec<(usize, usize)> {
    let mut remaining = vec![(start, start.saturating_add(size))];
    let mut overlaps = existing_regions.to_vec();
    overlaps.sort_by_key(|region| region.gpa);

    for region in overlaps {
        let overlap_start = region.gpa;
        let overlap_end = region.gpa.saturating_add(region.size);
        let mut next_remaining = Vec::new();

        for (seg_start, seg_end) in remaining {
            if overlap_end <= seg_start || overlap_start >= seg_end {
                next_remaining.push((seg_start, seg_end));
                continue;
            }

            if seg_start < overlap_start {
                next_remaining.push((seg_start, overlap_start.min(seg_end)));
            }
            if overlap_end < seg_end {
                next_remaining.push((overlap_end.max(seg_start), seg_end));
            }
        }

        remaining = next_remaining;
        if remaining.is_empty() {
            break;
        }
    }

    remaining
        .into_iter()
        .filter_map(|(seg_start, seg_end)| {
            let seg_size = seg_end.saturating_sub(seg_start);
            (seg_size > 0).then_some((seg_start, seg_size))
        })
        .collect()
}

fn reserved_memory_regions(crate_cfg: &AxVMCrateConfig) -> impl Iterator<Item = &VmMemConfig> {
    crate_cfg
        .kernel
        .memory_regions
        .iter()
        .filter(|region| region.map_type == VmMemMappingType::MapReserved)
}

fn excluded_device_paths(crate_cfg: &AxVMCrateConfig) -> Vec<String> {
    crate_cfg
        .devices
        .excluded_devices
        .iter()
        .flatten()
        .cloned()
        .collect()
}

fn is_excluded_node_path(node_path: &str, excluded_paths: &[String]) -> bool {
    excluded_paths.iter().any(|excluded| {
        node_path == excluded
            || node_path
                .strip_prefix(excluded)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn push_reserved_address_range(
    ranges: &mut Vec<ReservedAddressConfig>,
    node_path: &str,
    base: usize,
    size: usize,
) {
    let Some((base_gpa, length)) = align_reserved_region_4k(base, size) else {
        return;
    };

    let mut merged = ReservedAddressConfig { base_gpa, length };
    let mut index = 0;
    while index < ranges.len() {
        let existing = &ranges[index];
        let merged_end = merged.base_gpa.saturating_add(merged.length);
        let existing_end = existing.base_gpa.saturating_add(existing.length);
        if merged.base_gpa <= existing_end && existing.base_gpa <= merged_end {
            let merged_base = merged.base_gpa.min(existing.base_gpa);
            let merged_end = merged_end.max(existing_end);
            merged = ReservedAddressConfig {
                base_gpa: merged_base,
                length: merged_end.saturating_sub(merged_base),
            };
            ranges.remove(index);
        } else {
            index += 1;
        }
    }

    debug!(
        "Reserving excluded device {} range [{:#x}~{:#x}] from passthrough mapping",
        node_path,
        merged.base_gpa,
        merged.base_gpa.saturating_add(merged.length)
    );
    ranges.push(merged);
}

fn node_regs(fdt: &Fdt, node_id: usize) -> Vec<fdt_edit::RegFixed> {
    fdt.view_typed(node_id)
        .map(|node| node.regs())
        .unwrap_or_default()
}

fn node_pci_ranges(fdt: &Fdt, node_id: usize) -> Vec<PciRange> {
    match fdt.view_typed(node_id) {
        Some(NodeType::Pci(pci)) => pci.ranges().unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn reserve_excluded_device_ranges(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &AxVMCrateConfig,
    dtb: &[u8],
) -> AxVmResult {
    let excluded_paths = excluded_device_paths(crate_cfg);
    if excluded_paths.is_empty() {
        return Ok(());
    }

    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading excluded devices: {e:#?}")
        )
    })?;
    let mut reserved_ranges = Vec::new();

    for node_id in fdt.iter_node_ids() {
        let node_path = fdt.path_of(node_id);
        if !is_excluded_node_path(&node_path, &excluded_paths) {
            continue;
        }

        for reg in node_regs(&fdt, node_id) {
            push_reserved_address_range(
                &mut reserved_ranges,
                &node_path,
                reg.address as usize,
                reg.size.unwrap_or(0) as usize,
            );
        }

        for range in node_pci_ranges(&fdt, node_id) {
            push_reserved_address_range(
                &mut reserved_ranges,
                &node_path,
                range.cpu_address as usize,
                range.size as usize,
            );
        }
    }

    reserved_ranges.sort_by_key(|range| range.base_gpa);
    for range in reserved_ranges {
        vm_cfg.add_reserved_address_range(range);
    }

    Ok(())
}

fn is_memory_like_compatible(node: &Node) -> bool {
    node.compatibles().any(|compat| {
        compat == "mmio-sram"
            || compat.contains("shared-memory")
            || compat.contains("shmem")
            || compat.contains("sram")
    })
}

fn is_partition_like_node(node: &Node, node_path: &str) -> bool {
    node.compatibles()
        .any(|compat| compat == "fixed-partitions")
        || node_path.contains("/partitions/")
}

fn should_skip_passthrough_node(
    fdt: &Fdt,
    node_id: usize,
    node: &Node,
    node_path: &str,
    reserved_regions: &[VmMemConfig],
) -> bool {
    if !is_memory_like_compatible(node) {
        return false;
    }

    for reg in node_regs(fdt, node_id) {
        let gpa = reg.address as usize;
        let size = reg.size.unwrap_or(0) as usize;
        if size == 0 {
            continue;
        }

        if let Some(region) = reserved_regions
            .iter()
            .find(|region| overlaps_memory_region(gpa, size, region))
        {
            debug!(
                "Skipping passthrough node {} [{:#x}~{:#x}] because memory-like compatible \
                 overlaps reserved region [{:#x}~{:#x}]",
                node_path,
                gpa,
                gpa + size,
                region.gpa,
                region.gpa + region.size
            );
            return true;
        }
    }

    false
}

pub fn parse_reserved_memory_regions(crate_cfg: &mut AxVMCrateConfig, dtb: &[u8]) -> AxVmResult {
    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading reserved memory: {e:#?}")
        )
    })?;
    let default_flags = (MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE).bits();

    let mut added_count = 0usize;
    for node_id in fdt.iter_node_ids() {
        let node_path = fdt.path_of(node_id);
        if !is_reserved_memory_path(&node_path) {
            continue;
        }

        for reg in node_regs(&fdt, node_id) {
            let original_gpa = reg.address as usize;
            let original_size = reg.size.unwrap_or(0) as usize;
            let Some((gpa, size)) = align_reserved_region_4k(original_gpa, original_size) else {
                continue;
            };

            let remaining_segments =
                subtract_memory_region_overlap(gpa, size, &crate_cfg.kernel.memory_regions);

            for (seg_gpa, seg_size) in remaining_segments {
                crate_cfg.kernel.memory_regions.push(VmMemConfig {
                    gpa: seg_gpa,
                    size: seg_size,
                    flags: default_flags,
                    map_type: VmMemMappingType::MapReserved,
                });
                added_count += 1;
            }
        }
    }

    if added_count > 0 {
        debug!(
            "Added {} reserved-memory region(s) from DTB into VM kernel memory_regions",
            added_count
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct FdtCpuNode {
    unit_address: usize,
    hardware_id: usize,
}

pub fn set_phys_cpu_sets(
    vm_cfg: &mut AxVMConfig,
    fdt: &Fdt,
    crate_config: &AxVMCrateConfig,
) -> AxVmResult {
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_ref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;

    let cpu_nodes_info: Vec<_> = fdt
        .iter_node_ids()
        .filter_map(|node_id| {
            let path = fdt.path_of(node_id);
            let node_id_from_path = path
                .strip_prefix("/cpus/cpu@")
                .and_then(|id| id.split('/').next())
                .and_then(|id| usize::from_str_radix(id, 16).ok())?;
            let guest_cpu_id = node_regs(fdt, node_id).first()?.address as usize;
            info!(
                "CPU node: {}, node_id: 0x{:x}, guest_cpu_id: 0x{:x}",
                path, node_id_from_path, guest_cpu_id
            );
            Some(FdtCpuNode {
                unit_address: node_id_from_path,
                hardware_id: guest_cpu_id,
            })
        })
        .collect();
    info!("Found {} host CPU nodes", cpu_nodes_info.len());

    let mut new_phys_cpu_sets = Vec::new();
    let mut guest_phys_cpu_ids = Vec::new();
    for phys_cpu_id in phys_cpu_ids {
        if let Some(cpu_node) = cpu_nodes_info
            .iter()
            .find(|cpu_node| cpu_node.unit_address == *phys_cpu_id)
        {
            let cpu_mask = cpu_mask_from_fdt_cpu_node(*cpu_node, |hardware_id| {
                ax_std::os::arceos::modules::ax_hal::power::cpu_id_to_idx(hardware_id)
            })
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidInput,
                    format!(
                        "CPU node cpu@{phys_cpu_id:x} has hardware ID {:#x}, which has no runtime \
                         logical CPU mapping",
                        cpu_node.hardware_id
                    )
                )
            })?;
            new_phys_cpu_sets.push(cpu_mask);
            guest_phys_cpu_ids.push(cpu_node.hardware_id);
        } else {
            error!(
                "vCPU {} with phys_cpu_id 0x{:x} not found in device tree!",
                vm_cfg.id(),
                phys_cpu_id
            );
        }
    }

    let phys_cpu_ls = vm_cfg.phys_cpu_ls_mut();
    phys_cpu_ls.set_guest_cpu_sets(new_phys_cpu_sets);
    phys_cpu_ls.set_guest_phys_cpu_ids(guest_phys_cpu_ids);
    Ok(())
}

fn cpu_mask_from_fdt_cpu_node(
    cpu_node: FdtCpuNode,
    cpu_id_to_idx: impl FnOnce(usize) -> Option<usize>,
) -> Option<usize> {
    cpu_mask_from_hardware_id(cpu_node.hardware_id, cpu_id_to_idx)
}

fn cpu_mask_from_hardware_id(
    hardware_id: usize,
    cpu_id_to_idx: impl FnOnce(usize) -> Option<usize>,
) -> Option<usize> {
    cpu_id_to_idx(hardware_id)
        .and_then(|cpu_idx| u32::try_from(cpu_idx).ok())
        .and_then(|cpu_idx| 1usize.checked_shl(cpu_idx))
}

fn add_device_address_config(
    vm_cfg: &mut AxVMConfig,
    node_name: &str,
    base_address: usize,
    size: usize,
    index: usize,
    prefix: Option<&str>,
) {
    if size == 0 {
        return;
    }

    let addr_end = base_address.saturating_add(size);
    if let Some(emu_dev) = vm_cfg.emu_devices().iter().find(|emu_dev| {
        let emu_start = emu_dev.base_gpa;
        let emu_end = emu_dev.base_gpa.saturating_add(emu_dev.length);
        base_address < emu_end && emu_start < addr_end
    }) {
        debug!(
            "Skipping passthrough mapping for node {} [{:#x}~{:#x}] because it overlaps emulated \
             device {} [{:#x}~{:#x}]",
            node_name,
            base_address,
            addr_end,
            emu_dev.name,
            emu_dev.base_gpa,
            emu_dev.base_gpa.saturating_add(emu_dev.length),
        );
        return;
    }

    let device_name = if index == 0 {
        match prefix {
            Some(p) => format!("{node_name}-{p}"),
            None => node_name.to_string(),
        }
    } else {
        match prefix {
            Some(p) => format!("{node_name}-{p}-region{index}"),
            None => format!("{node_name}-region{index}"),
        }
    };

    vm_cfg.add_pass_through_device(PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    });
}

fn add_pci_ranges_config(vm_cfg: &mut AxVMConfig, node_name: &str, range: &PciRange, index: usize) {
    let base_address = range.cpu_address as usize;
    let size = range.size as usize;

    if size == 0 {
        return;
    }

    let prefix = match range.space {
        PciSpace::IO => "io",
        PciSpace::Memory32 => "mem32",
        PciSpace::Memory64 => "mem64",
    };

    let device_name = if index == 0 {
        format!("{node_name}-{prefix}")
    } else {
        format!("{node_name}-{prefix}-region{index}")
    };

    vm_cfg.add_pass_through_device(PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    });
}

pub fn parse_passthrough_devices_address(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &AxVMCrateConfig,
    dtb: &[u8],
) -> AxVmResult {
    let devices = vm_cfg.pass_through_devices().to_vec();
    if !devices.is_empty() && devices[0].length != 0 {
        for (index, device) in devices.iter().enumerate() {
            add_device_address_config(
                vm_cfg,
                &device.name,
                device.base_gpa,
                device.length,
                index,
                None,
            );
        }
        return Ok(());
    }

    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading passthrough devices: {e:#?}")
        )
    })?;

    vm_cfg.clear_pass_through_devices();
    let reserved_regions: Vec<VmMemConfig> = reserved_memory_regions(crate_cfg).cloned().collect();

    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let node_path = fdt.path_of(node_id);

        if node_path == "/"
            || node.name().starts_with("memory")
            || is_reserved_memory_path(&node_path)
        {
            continue;
        }

        if is_partition_like_node(node, &node_path)
            || should_skip_passthrough_node(&fdt, node_id, node, &node_path, &reserved_regions)
        {
            continue;
        }

        let node_name = node.name().to_string();
        if node_name.starts_with("pcie@") || node_name.contains("pci") {
            for (index, range) in node_pci_ranges(&fdt, node_id).iter().enumerate() {
                add_pci_ranges_config(vm_cfg, &node_name, range, index);
            }

            for (index, reg) in node_regs(&fdt, node_id).iter().enumerate() {
                add_device_address_config(
                    vm_cfg,
                    &node_name,
                    reg.address as usize,
                    reg.size.unwrap_or(0) as usize,
                    index,
                    Some("ecam"),
                );
            }
        } else {
            for (index, reg) in node_regs(&fdt, node_id).iter().enumerate() {
                add_device_address_config(
                    vm_cfg,
                    &node_name,
                    reg.address as usize,
                    reg.size.unwrap_or(0) as usize,
                    index,
                    None,
                );
            }
        }
    }
    Ok(())
}

pub fn parse_vm_interrupt(vm_cfg: &mut AxVMConfig, dtb: &[u8]) -> AxVmResult {
    let decode_interrupt = super::selected_guest_fdt_policy().decode_interrupt;
    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading interrupts: {e:#?}")
        )
    })?;

    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let name = node.name();
        if name.starts_with("memory")
            || name.starts_with("interrupt-controller")
            || name.starts_with("intc")
            || name.starts_with("its")
        {
            continue;
        }

        let Some(view) = fdt.view_typed(node_id) else {
            continue;
        };
        for interrupt in view.interrupts() {
            if let Some(irq) = decode_interrupt(&interrupt.specifier) {
                trace!("node: {name}, passthrough interrupt id: 0x{irq:x}");
                vm_cfg.add_pass_through_irq(irq);
            }
        }
    }

    Ok(())
}

pub fn update_provided_fdt(
    provided_dtb: &[u8],
    host_dtb: Option<&[u8]>,
    crate_config: &AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    let patch_provided = super::selected_guest_fdt_policy().patch_provided;
    patch_provided(provided_dtb, host_dtb, crate_config)
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec, vec::Vec};

    use axvm_types::{AddressSpacePolicy, VmMemConfig, VmMemMappingType};
    use axvmconfig::{AxVMCrateConfig, VMDevicesConfig};
    use fdt_edit::{Fdt, Node};
    use fdt_raw::RegInfo;

    use super::{
        FdtCpuNode, align_reserved_region_4k, cpu_mask_from_fdt_cpu_node,
        reserve_excluded_device_ranges,
    };
    use crate::config::{AxVMConfig, AxVMConfigParams, PhysCpuList};

    fn prop_u32(name: &str, value: u32) -> fdt_edit::Property {
        let mut prop = fdt_edit::Property::new(name, alloc::vec![]);
        prop.set_u32_ls(&[value]);
        prop
    }

    fn fdt_with_excluded_devices() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        for (name, base, size) in [
            ("serial@10001234", 0x1000_1234, 0x100),
            ("gpio@10002000", 0x1000_2000, 0x1000),
        ] {
            let node = fdt.add_node(root, Node::new(name));
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(size))]);
        }

        fdt.encode().as_ref().to_vec()
    }

    #[test]
    fn align_reserved_region_keeps_aligned_range() {
        assert_eq!(
            align_reserved_region_4k(0x1000, 0x2000),
            Some((0x1000, 0x2000))
        );
    }

    #[test]
    fn align_reserved_region_expands_to_cover_unaligned_bounds() {
        assert_eq!(
            align_reserved_region_4k(0x1100, 0x2500),
            Some((0x1000, 0x3000))
        );
    }

    #[test]
    fn align_reserved_region_rejects_zero_sized_range() {
        assert_eq!(align_reserved_region_4k(0x1000, 0), None);
    }

    #[test]
    fn fdt_cpu_node_reg_uses_runtime_logical_mapping() {
        let cpu_node = FdtCpuNode {
            unit_address: 0x100,
            hardware_id: 0,
        };
        let runtime_hardware_ids = [0, 0x200, 0x201, 0x100];

        let cpu_mask = cpu_mask_from_fdt_cpu_node(cpu_node, |hardware_id| {
            runtime_hardware_ids
                .iter()
                .position(|id| *id == hardware_id)
        });

        assert_eq!(cpu_mask, Some(1));
    }

    #[test]
    fn subtract_memory_region_overlap_keeps_non_overlapping_range() {
        let existing = vec![VmMemConfig {
            gpa: 0x4000,
            size: 0x1000,
            flags: 0,
            map_type: VmMemMappingType::MapReserved,
        }];

        assert_eq!(
            super::subtract_memory_region_overlap(0x1000, 0x1000, &existing),
            vec![(0x1000, 0x1000)]
        );
    }

    #[test]
    fn subtract_memory_region_overlap_splits_range_around_overlap() {
        let existing = vec![VmMemConfig {
            gpa: 0x3000,
            size: 0x2000,
            flags: 0,
            map_type: VmMemMappingType::MapReserved,
        }];

        assert_eq!(
            super::subtract_memory_region_overlap(0x1000, 0x6000, &existing),
            vec![(0x1000, 0x2000), (0x5000, 0x2000)]
        );
    }

    #[test]
    fn subtract_memory_region_overlap_drops_fully_covered_range() {
        let existing = vec![VmMemConfig {
            gpa: 0x1000,
            size: 0x4000,
            flags: 0,
            map_type: VmMemMappingType::MapReserved,
        }];

        assert!(super::subtract_memory_region_overlap(0x2000, 0x1000, &existing).is_empty());
    }

    #[test]
    fn excluded_device_ranges_become_reserved_vm_ranges() {
        let dtb = fdt_with_excluded_devices();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            ..Default::default()
        });
        let crate_cfg = AxVMCrateConfig {
            devices: VMDevicesConfig {
                address_space_policy: AddressSpacePolicy::Passthrough,
                excluded_devices: vec![vec!["/serial@10001234".to_string()]],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        let ranges = vm_cfg.reserved_address_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].base_gpa, 0x1000_1000);
        assert_eq!(ranges[0].length, 0x1000);
    }
}
