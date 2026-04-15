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

//! FDT parsing and processing functionality.

use alloc::{string::ToString, vec::Vec};
use ax_hal::{dtb, mem};
use axaddrspace::MappingFlags;
use axvm::config::{
    AxVMConfig, AxVMCrateConfig, PassThroughDeviceConfig, VmMemConfig, VmMemMappingType,
};
use fdt_parser::{Fdt, FdtHeader, PciRange, PciSpace};

use crate::vmm::fdt::crate_guest_fdt_with_cache;
#[cfg(not(target_arch = "riscv64"))]
use crate::vmm::fdt::create::update_cpu_node;

const PAGE_SIZE_4K: usize = 0x1000;

pub fn get_host_fdt() -> &'static [u8] {
    const FDT_VALID_MAGIC: u32 = 0xd00d_feed;
    let bootarg: usize = dtb::get_bootarg();
    let fdt_vaddr = mem::phys_to_virt(bootarg.into());
    let header = unsafe {
        core::slice::from_raw_parts(fdt_vaddr.as_ptr(), core::mem::size_of::<FdtHeader>())
    };
    let fdt_header = FdtHeader::from_bytes(header)
        .map_err(|e| format!("Failed to parse FDT header: {e:#?}"))
        .unwrap();

    if fdt_header.magic.get() != FDT_VALID_MAGIC {
        error!(
            "FDT magic is invalid, expected {:#x}, got {:#x}",
            FDT_VALID_MAGIC,
            fdt_header.magic.get()
        );
    }

    unsafe { core::slice::from_raw_parts(fdt_vaddr.as_ptr(), fdt_header.total_size()) }
}

pub fn setup_guest_fdt_from_vmm(
    fdt_bytes: &[u8],
    vm_cfg: &mut AxVMConfig,
    crate_config: &AxVMCrateConfig,
) {
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");

    // Call the modified function and get the returned device name list
    let passthrough_device_names = super::device::find_all_passthrough_devices(vm_cfg, &fdt);

    let dtb_data = super::create::crate_guest_fdt(&fdt, &passthrough_device_names, crate_config);
    crate_guest_fdt_with_cache(dtb_data, crate_config);
}

fn is_reserved_memory_path(node_path: &str) -> bool {
    node_path == "/reserved-memory" || node_path.starts_with("/reserved-memory/")
}

fn overlaps_memory_region(lhs_gpa: usize, lhs_size: usize, rhs: &VmMemConfig) -> bool {
    let lhs_end = lhs_gpa.saturating_add(lhs_size);
    let rhs_end = rhs.gpa.saturating_add(rhs.size);
    lhs_gpa < rhs_end && rhs.gpa < lhs_end
}

fn is_covered_by_memory_region(gpa: usize, size: usize, region: &VmMemConfig) -> bool {
    let end = gpa.saturating_add(size);
    let region_end = region.gpa.saturating_add(region.size);
    region.gpa <= gpa && region_end >= end
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

fn is_memory_like_compatible(node: &fdt_parser::Node<'_>) -> bool {
    node.compatibles().any(|compat| {
        compat == "mmio-sram"
            || compat.contains("shared-memory")
            || compat.contains("shmem")
            || compat.contains("sram")
    })
}

fn is_partition_like_node(node: &fdt_parser::Node<'_>, node_path: &str) -> bool {
    if node
        .compatibles()
        .any(|compat| compat == "fixed-partitions")
    {
        return true;
    }

    node_path.contains("/partitions/")
}

fn should_skip_passthrough_node(
    node: &fdt_parser::Node<'_>,
    node_path: &str,
    reserved_regions: &[VmMemConfig],
) -> bool {
    if !is_memory_like_compatible(node) {
        return false;
    }

    let Some(reg_iter) = node.reg() else {
        return false;
    };

    for reg in reg_iter {
        let gpa = reg.address as usize;
        let size = reg.size.unwrap_or(0);
        if size == 0 {
            continue;
        }

        if let Some(region) = reserved_regions
            .iter()
            .find(|region| overlaps_memory_region(gpa, size, region))
        {
            debug!(
                "Skipping passthrough node {} [{:#x}~{:#x}] because memory-like compatible overlaps reserved region [{:#x}~{:#x}]",
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

pub fn parse_reserved_memory_regions(crate_cfg: &mut AxVMCrateConfig, dtb: &[u8]) {
    let fdt = Fdt::from_bytes(dtb)
        .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");
    let all_nodes: Vec<_> = fdt.all_nodes().collect();
    let all_paths = super::build_all_node_paths(&all_nodes);
    let default_flags = (MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE).bits();

    let mut added_count = 0usize;
    for (index, node) in all_nodes.iter().enumerate() {
        let node_path = &all_paths[index];
        if !is_reserved_memory_path(&node_path) {
            continue;
        }

        if let Some(reg_iter) = node.reg() {
            for reg in reg_iter {
                let original_gpa = reg.address as usize;
                let original_size = reg.size.unwrap_or(0);
                let Some((gpa, size)) = align_reserved_region_4k(original_gpa, original_size)
                else {
                    continue;
                };

                if gpa != original_gpa || size != original_size {
                    debug!(
                        "Aligning reserved-memory {} from [{:#x}~{:#x}] to [{:#x}~{:#x}]",
                        node_path,
                        original_gpa,
                        original_gpa.saturating_add(original_size),
                        gpa,
                        gpa.saturating_add(size)
                    );
                }

                let remaining_segments =
                    subtract_memory_region_overlap(gpa, size, &crate_cfg.kernel.memory_regions);

                if remaining_segments.is_empty() {
                    debug!(
                        "Skipping reserved-memory {} [{:#x}~{:#x}] because it is fully covered by existing memory_regions",
                        node_path,
                        gpa,
                        gpa + size
                    );
                    continue;
                }

                if remaining_segments.len() != 1 || remaining_segments[0] != (gpa, size) {
                    debug!(
                        "Cropping reserved-memory {} [{:#x}~{:#x}] into {:?} to avoid overlaps",
                        node_path,
                        gpa,
                        gpa + size,
                        remaining_segments
                    );
                }

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
    }

    if added_count > 0 {
        debug!(
            "Added {} reserved-memory region(s) from DTB into VM kernel memory_regions",
            added_count
        );
    }
}

#[cfg(test)]
mod tests {
    use super::align_reserved_region_4k;

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
    fn subtract_memory_region_overlap_keeps_non_overlapping_range() {
        let existing = vec![VmMemConfig {
            gpa: 0x4000,
            size: 0x1000,
            flags: 0,
            map_type: VmMemMappingType::MapReserved,
        }];

        assert_eq!(
            subtract_memory_region_overlap(0x1000, 0x1000, &existing),
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
            subtract_memory_region_overlap(0x1000, 0x6000, &existing),
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

        assert!(subtract_memory_region_overlap(0x2000, 0x1000, &existing).is_empty());
    }
}

pub fn set_phys_cpu_sets(vm_cfg: &mut AxVMConfig, fdt: &Fdt, crate_config: &AxVMCrateConfig) {
    // Find and parse CPU information from host DTB
    let host_cpus: Vec<_> = fdt.find_nodes("/cpus/cpu").collect();
    info!("Found {} host CPU nodes", &host_cpus.len());

    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_ref()
        .expect("ERROR: phys_cpu_ids not found in config.toml");

    // Collect all CPU node information into Vec to avoid using iterators multiple times
    let cpu_nodes_info: Vec<_> = host_cpus
        .iter()
        .filter_map(|cpu_node| {
            if let Some(mut cpu_reg) = cpu_node.reg() {
                if let Some(r) = cpu_reg.next() {
                    info!(
                        "CPU node: {}, phys_cpu_id: 0x{:x}",
                        cpu_node.name(),
                        r.address
                    );
                    Some((cpu_node.name().to_string(), r.address as usize))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();
    // Create mapping from phys_cpu_id to physical CPU index
    // Collect all unique CPU addresses, maintaining the order of appearance in the device tree
    let mut unique_cpu_addresses = Vec::new();
    for (_, cpu_address) in &cpu_nodes_info {
        if !unique_cpu_addresses.contains(cpu_address) {
            unique_cpu_addresses.push(*cpu_address);
        } else {
            panic!("Duplicate CPU address found");
        }
    }

    // Assign index to each CPU address in the device tree and print detailed information
    for (index, &cpu_address) in unique_cpu_addresses.iter().enumerate() {
        // Find all CPU nodes using this address
        for (cpu_name, node_address) in &cpu_nodes_info {
            if *node_address == cpu_address {
                debug!(
                    "  CPU node: {cpu_name}, address: 0x{cpu_address:x}, assigned index: {index}"
                );
                break; // Print each address only once
            }
        }
    }

    // Calculate phys_cpu_sets based on phys_cpu_ids in vcpu_mappings
    let mut new_phys_cpu_sets = Vec::new();
    for phys_cpu_id in phys_cpu_ids {
        // Find the index corresponding to phys_cpu_id in unique_cpu_addresses
        if let Some(cpu_index) = unique_cpu_addresses
            .iter()
            .position(|&addr| addr == *phys_cpu_id)
        {
            let cpu_mask = 1usize << cpu_index; // Convert index to mask bit
            new_phys_cpu_sets.push(cpu_mask);
            debug!(
                "vCPU {} with phys_cpu_id 0x{:x} mapped to CPU index {} (mask: 0x{:x})",
                vm_cfg.id(),
                phys_cpu_id,
                cpu_index,
                cpu_mask
            );
        } else {
            error!(
                "vCPU {} with phys_cpu_id 0x{:x} not found in device tree!",
                vm_cfg.id(),
                phys_cpu_id
            );
        }
    }

    // Update phys_cpu_sets in VM configuration (if VM configuration supports setting)
    info!("Calculated phys_cpu_sets: {new_phys_cpu_sets:?}");

    vm_cfg
        .phys_cpu_ls_mut()
        .set_guest_cpu_sets(new_phys_cpu_sets);

    debug!(
        "vcpu_mappings: {:?}",
        vm_cfg.phys_cpu_ls_mut().get_vcpu_affinities_pcpu_ids()
    );
}

/// Add address mapping configuration for a device
fn add_device_address_config(
    vm_cfg: &mut AxVMConfig,
    node_name: &str,
    base_address: usize,
    size: usize,
    index: usize,
    prefix: Option<&str>,
) {
    // Only process devices with address information
    if size == 0 {
        return;
    }

    // Create a device configuration for each address segment
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

    // Add new device configuration
    let pt_dev = axvm::config::PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    };
    vm_cfg.add_pass_through_device(pt_dev);
}

/// Add ranges property configuration for PCIe devices
fn add_pci_ranges_config(vm_cfg: &mut AxVMConfig, node_name: &str, range: &PciRange, index: usize) {
    let base_address = range.cpu_address as usize;
    let size = range.size as usize;

    // Only process devices with address information
    if size == 0 {
        return;
    }

    // Create a device configuration for each address segment
    let prefix = match range.space {
        PciSpace::Configuration => "config",
        PciSpace::IO => "io",
        PciSpace::Memory32 => "mem32",
        PciSpace::Memory64 => "mem64",
    };

    let device_name = if index == 0 {
        format!("{node_name}-{prefix}")
    } else {
        format!("{node_name}-{prefix}-region{index}")
    };

    // Add new device configuration
    let pt_dev = axvm::config::PassThroughDeviceConfig {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
        irq_id: 0,
    };
    vm_cfg.add_pass_through_device(pt_dev);

    trace!(
        "Added PCIe passthrough device {}: base=0x{:x}, size=0x{:x}, space={:?}",
        node_name, base_address, size, range.space
    );
}

pub fn parse_passthrough_devices_address(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &AxVMCrateConfig,
    dtb: &[u8],
) {
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
    } else {
        let fdt = Fdt::from_bytes(dtb)
            .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");

        // Clear existing passthrough device configurations
        vm_cfg.clear_pass_through_devices();

        let all_nodes: Vec<_> = fdt.all_nodes().collect();
        let all_paths = super::build_all_node_paths(&all_nodes);
        let reserved_regions: Vec<VmMemConfig> =
            reserved_memory_regions(crate_cfg).cloned().collect();

        // Traverse all device tree nodes
        for (index, node) in all_nodes.iter().enumerate() {
            let node_path = &all_paths[index];

            // Skip root node
            if node.name() == "/"
                || node.name().starts_with("memory")
                || is_reserved_memory_path(&node_path)
            {
                continue;
            }

            if is_partition_like_node(node, &node_path) {
                debug!(
                    "Skipping partition-like node {} from passthrough parsing",
                    node_path
                );
                continue;
            }

            if should_skip_passthrough_node(node, &node_path, &reserved_regions) {
                continue;
            }

            let node_name = node.name().to_string();

            // Check if it's a PCIe device node
            if node_name.starts_with("pcie@") || node_name.contains("pci") {
                // Process PCIe device's ranges property
                if let Some(pci) = node.clone().into_pci()
                    && let Ok(ranges) = pci.ranges()
                {
                    for (index, range) in ranges.enumerate() {
                        add_pci_ranges_config(vm_cfg, &node_name, &range, index);
                    }
                }

                // Process PCIe device's reg property (ECAM space)
                if let Some(reg_iter) = node.reg() {
                    for (index, reg) in reg_iter.enumerate() {
                        let base_address = reg.address as usize;
                        let size = reg.size.unwrap_or(0);

                        add_device_address_config(
                            vm_cfg,
                            &node_name,
                            base_address,
                            size,
                            index,
                            Some("ecam"),
                        );
                    }
                }
            } else {
                // Get device's reg property (process regular devices)
                if let Some(reg_iter) = node.reg() {
                    // Process all address segments of the device
                    for (index, reg) in reg_iter.enumerate() {
                        // Get device's address and size information
                        let base_address = reg.address as usize;
                        let size = reg.size.unwrap_or(0);

                        add_device_address_config(
                            vm_cfg,
                            &node_name,
                            base_address,
                            size,
                            index,
                            None,
                        );
                    }
                }
            }
        }
        trace!(
            "All passthrough devices: {:#x?}",
            vm_cfg.pass_through_devices()
        );
        debug!(
            "Finished parsing passthrough devices, total: {}",
            vm_cfg.pass_through_devices().len()
        );
    }
}

pub fn parse_vm_interrupt(vm_cfg: &mut AxVMConfig, dtb: &[u8]) {
    const GIC_PHANDLE: usize = 1;
    let fdt = Fdt::from_bytes(dtb)
        .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");

    for node in fdt.all_nodes() {
        let name = node.name();

        if name.starts_with("memory") {
            continue;
        }
        // Skip the interrupt controller, as we will use vGIC
        // TODO: filter with compatible property and parse its phandle from DT; maybe needs a second pass?
        else if name.starts_with("interrupt-controller")
            || name.starts_with("intc")
            || name.starts_with("its")
        {
            debug!("skipping node {name} to use vGIC");
            continue;
        }

        // Collect all GIC_SPI interrupts and add them to vGIC
        if let Some(interrupts) = node.interrupts() {
            // TODO: skip non-GIC interrupt
            if let Some(parent) = node.interrupt_parent() {
                trace!("node: {}, intr parent: {}", name, parent.node.name());
                if let Some(phandle) = parent.node.phandle() {
                    if phandle.as_usize() != GIC_PHANDLE {
                        debug!(
                            "node: {}, intr parent: {}, phandle: 0x{:x} is not GIC!",
                            name,
                            parent.node.name(),
                            phandle.as_usize()
                        );
                    }
                } else {
                    warn!(
                        "node: {}, intr parent: {} no phandle!",
                        name,
                        parent.node.name(),
                    );
                }
            } else {
                warn!("node: {name} no interrupt parent!");
            }

            for interrupt in interrupts {
                // <GIC_SPI/GIC_PPI, IRQn, trigger_mode>
                for (k, v) in interrupt.enumerate() {
                    match k {
                        0 => {
                            if v == 0 {
                                trace!("node: {name}, GIC_SPI");
                            } else {
                                debug!("node: {name}, intr type: {v}, not GIC_SPI, not supported!");
                                break;
                            }
                        }
                        1 => {
                            trace!("node: {name}, interrupt id: 0x{v:x}");
                            vm_cfg.add_pass_through_spi(v);
                        }
                        2 => {
                            trace!("node: {name}, interrupt mode: 0x{v:x}");
                        }
                        _ => {
                            warn!("unknown interrupt property {k}:0x{v:x}")
                        }
                    }
                }
            }
        }
    }

    // vm_cfg.add_pass_through_device(PassThroughDeviceConfig {
    //     name: "Fake Node".to_string(),
    //     base_gpa: 0x0,
    //     base_hpa: 0x0,
    //     length: 0x20_0000,
    //     irq_id: 0,
    // });
}

pub fn update_provided_fdt(provided_dtb: &[u8], host_dtb: &[u8], crate_config: &AxVMCrateConfig) {
    #[cfg(target_arch = "riscv64")]
    {
        let _ = host_dtb;
        crate_guest_fdt_with_cache(provided_dtb.to_vec(), crate_config);
    }

    #[cfg(not(target_arch = "riscv64"))]
    {
        let provided_fdt = Fdt::from_bytes(provided_dtb)
            .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");
        let host_fdt = Fdt::from_bytes(host_dtb)
            .expect("Failed to parse DTB image, perhaps the DTB is invalid or corrupted");
        let provided_dtb_data = update_cpu_node(&provided_fdt, &host_fdt, crate_config);
        crate_guest_fdt_with_cache(provided_dtb_data, crate_config);
    }
}
