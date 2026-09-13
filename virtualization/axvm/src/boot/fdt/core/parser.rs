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

use std::{
    collections::BTreeSet,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use axvmconfig::{
    GuestConfig, HostDeviceAssignment, ReservedAddressConfig, VmMemConfig, VmMemMappingType,
};
use fdt_edit::{Fdt, Node, NodeType, PciRange, PciSpace};

use super::policy::DecodedInterrupt;
use crate::{config::*, *};

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
    crate_config: &GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| ax_err_type!(InvalidData, format!("Failed to parse host FDT: {e:#?}")))?;

    // The runtime configuration may contain an implicit root selector to
    // establish the passthrough address-space policy. Keep its non-PCI
    // devices (for example the guest-owned virtio-blk endpoint), but remove
    // PCI host bridges unless the guest explicitly claims one.
    let explicit_device_names = crate_config
        .devices
        .passthrough
        .iter()
        .map(|device| device.path.clone())
        .collect::<Vec<_>>();
    let implicit_root_passthrough = explicit_device_names.is_empty()
        && vm_cfg
            .pass_through_devices()
            .iter()
            .any(|device| device.name == "/");
    let selected_device_names = if implicit_root_passthrough {
        vm_cfg
            .pass_through_devices()
            .iter()
            .map(|device| device.name.clone())
            .collect::<Vec<_>>()
    } else {
        explicit_device_names
    };
    if implicit_root_passthrough {
        for node_id in fdt.iter_node_ids() {
            if fdt.node(node_id).is_some_and(Node::is_pci) {
                vm_cfg.exclude_device_path(fdt.path_of(node_id));
            }
        }
    }
    reserve_excluded_device_ranges(vm_cfg, crate_config, fdt_bytes)?;
    let excluded_device_paths = vm_cfg
        .excluded_devices()
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let passthrough_device_names = super::device::find_all_passthrough_devices_from_paths(
        &selected_device_names,
        &excluded_device_paths,
        &fdt,
    );
    super::create::create_guest_fdt(
        &fdt,
        &passthrough_device_names,
        crate_config,
        &excluded_device_paths,
    )
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

fn reserved_memory_regions(crate_cfg: &GuestConfig) -> impl Iterator<Item = &VmMemConfig> {
    crate_cfg
        .kernel
        .memory_regions
        .iter()
        .filter(|region| region.map_type == VmMemMappingType::MapReserved)
}

fn excluded_device_paths(vm_cfg: &AxVMConfig, crate_cfg: &GuestConfig) -> Vec<String> {
    let mut paths = vm_cfg
        .excluded_devices()
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    paths.extend(
        crate_cfg
            .devices
            .disabled
            .iter()
            .map(|device| device.path.clone()),
    );
    paths.sort();
    paths.dedup();
    paths
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
    crate_cfg: &GuestConfig,
    dtb: &[u8],
) -> AxVmResult {
    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading excluded devices: {e:#?}")
        )
    })?;
    protect_machine_owned_firmware_devices(vm_cfg, crate_cfg, &fdt)?;
    let excluded_paths = excluded_device_paths(vm_cfg, crate_cfg);
    if excluded_paths.is_empty() {
        return Ok(());
    }
    let mut reserved_ranges = Vec::new();
    let decode_interrupt = super::selected_guest_fdt_policy().decode_interrupt;

    for node_id in fdt.iter_node_ids() {
        let node_path = fdt.path_of(node_id);
        if !is_excluded_node_path(&node_path, &excluded_paths) {
            continue;
        }

        exclude_node_interrupt_sources(vm_cfg, &fdt, node_id, decode_interrupt);
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

fn exclude_node_interrupt_sources(
    vm_cfg: &mut AxVMConfig,
    fdt: &Fdt,
    node_id: usize,
    decode_interrupt: fn(&[u32]) -> Option<DecodedInterrupt>,
) {
    let Some(view) = fdt.view_typed(node_id) else {
        return;
    };
    for interrupt in view.interrupts() {
        if let Some(interrupt) = decode_interrupt(&interrupt.specifier) {
            vm_cfg.exclude_pass_through_irq_source(interrupt.source);
        }
    }
}

fn protect_machine_owned_firmware_devices(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &GuestConfig,
    fdt: &Fdt,
) -> AxVmResult {
    let selected_paths = crate_cfg
        .devices
        .passthrough
        .iter()
        .map(|device| device.path.as_str())
        .collect::<Vec<_>>();
    let console_paths = super::serial::host_owned_serial_paths(fdt);
    if let Some(selected) = crate_cfg.devices.passthrough.iter().find(|selected| {
        console_paths
            .iter()
            .any(|console_path| super::device::selector_includes_path(&selected.path, console_path))
    }) {
        return Err(AxVmError::HostOwnedDevice {
            path: selected.path.clone(),
        });
    }
    let mut host_owned_paths = super::serial::physical_serial_paths(fdt);
    host_owned_paths.retain(|path| {
        console_paths.contains(path)
            || !selected_paths
                .iter()
                .any(|selector| super::device::selector_includes_path(selector, path))
    });
    host_owned_paths.extend(fdt.iter_node_ids().filter_map(|node_id| {
        let node = fdt.node(node_id)?;
        (is_machine_interrupt_controller(node) || super::timer::is_machine_timer_node(node))
            .then(|| fdt.path_of(node_id))
    }));
    host_owned_paths.sort();
    host_owned_paths.dedup();
    if let Some(selected) = crate_cfg
        .devices
        .passthrough
        .iter()
        .find(|selected| host_owned_paths.iter().any(|path| path == &selected.path))
    {
        return Err(AxVmError::HostOwnedDevice {
            path: selected.path.clone(),
        });
    }
    for path in host_owned_paths {
        vm_cfg.exclude_device_path(path);
    }
    Ok(())
}

fn is_machine_interrupt_controller(node: &Node) -> bool {
    if node.get_property("interrupt-controller").is_none() {
        return false;
    }
    node.name().starts_with("interrupt-controller")
        || node.name().starts_with("intc")
        || node.name().starts_with("its")
        || node.compatibles().any(|compatible| {
            compatible.contains("gic")
                || compatible.contains("plic")
                || compatible.contains("eiointc")
                || compatible.contains("extioi")
                || compatible.contains("liointc")
                || compatible.contains("pch-pic")
        })
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

pub fn parse_reserved_memory_regions(crate_cfg: &mut GuestConfig, dtb: &[u8]) -> AxVmResult {
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

pub fn set_phys_cpu_sets(
    vm_cfg: &mut AxVMConfig,
    fdt: &Fdt,
    crate_config: &GuestConfig,
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
            let hardware_cpu_id = node_regs(fdt, node_id).first()?.address as usize;
            info!(
                "CPU node: {}, node_id: 0x{:x}, hardware_cpu_id: 0x{:x}",
                path, node_id_from_path, hardware_cpu_id
            );
            Some((node_id_from_path, hardware_cpu_id))
        })
        .collect();
    info!("Found {} host CPU nodes", cpu_nodes_info.len());

    let policy = super::selected_guest_fdt_policy();
    let (new_phys_cpu_sets, guest_phys_cpu_ids) = resolve_phys_cpu_sets(
        phys_cpu_ids,
        &cpu_nodes_info,
        (policy.host_cpu_count)(),
        policy.resolve_cpu_index,
    )?;

    let phys_cpu_ls = vm_cfg.phys_cpu_ls_mut();
    phys_cpu_ls.set_guest_cpu_sets(new_phys_cpu_sets);
    phys_cpu_ls.set_guest_phys_cpu_ids(guest_phys_cpu_ids);
    Ok(())
}

fn resolve_phys_cpu_sets(
    phys_cpu_ids: &[usize],
    cpu_nodes: &[(usize, usize)],
    host_cpu_count: usize,
    mut resolve_cpu_index: impl FnMut(usize) -> Option<usize>,
) -> AxVmResult<(Vec<usize>, Vec<usize>)> {
    let mut cpu_sets = Vec::with_capacity(phys_cpu_ids.len());
    let mut guest_cpu_ids = Vec::with_capacity(phys_cpu_ids.len());

    for &phys_cpu_id in phys_cpu_ids {
        let &(_, hardware_cpu_id) = cpu_nodes
            .iter()
            .find(|(node_id, _)| *node_id == phys_cpu_id)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidInput,
                    format!("physical CPU ID 0x{phys_cpu_id:x} is missing from the host FDT")
                )
            })?;
        let logical_index = resolve_cpu_index(hardware_cpu_id).ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                format!(
                    "hardware CPU ID 0x{hardware_cpu_id:x} is missing from the runtime topology"
                )
            )
        })?;
        if logical_index >= host_cpu_count {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "logical CPU index {logical_index} is outside the {host_cpu_count} usable \
                     host CPUs"
                )
            ));
        }
        let cpu_mask = if logical_index < usize::BITS as usize {
            1usize << logical_index
        } else {
            return Err(ax_err_type!(
                InvalidInput,
                format!(
                    "logical CPU index {logical_index} does not fit the host CPU affinity mask"
                )
            ));
        };

        cpu_sets.push(cpu_mask);
        guest_cpu_ids.push(hardware_cpu_id);
    }

    Ok((cpu_sets, guest_cpu_ids))
}

fn add_device_address_config(
    vm_cfg: &mut AxVMConfig,
    node_path: &str,
    base_address: usize,
    size: usize,
    index: usize,
    prefix: Option<&str>,
) {
    if size == 0 {
        return;
    }

    let device_name = if index == 0 {
        match prefix {
            Some(p) => format!("{node_path}-{p}"),
            None => node_path.to_string(),
        }
    } else {
        match prefix {
            Some(p) => format!("{node_path}-{p}-region{index}"),
            None => format!("{node_path}-region{index}"),
        }
    };

    vm_cfg.add_pass_through_device(HostDeviceAssignment {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
    });
}

fn add_pci_ranges_config(vm_cfg: &mut AxVMConfig, node_path: &str, range: &PciRange, index: usize) {
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
        format!("{node_path}-{prefix}")
    } else {
        format!("{node_path}-{prefix}-region{index}")
    };

    vm_cfg.add_pass_through_device(HostDeviceAssignment {
        name: device_name,
        base_gpa: base_address,
        base_hpa: base_address,
        length: size,
    });
}

pub fn parse_passthrough_devices_address(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &GuestConfig,
    dtb: &[u8],
) -> AxVmResult {
    let devices = vm_cfg.pass_through_devices().to_vec();
    if devices.iter().all(|device| device.length != 0) {
        return Ok(());
    }

    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading passthrough devices: {e:#?}")
        )
    })?;

    let selected_paths = super::device::find_all_passthrough_devices(vm_cfg, &fdt)
        .into_iter()
        .collect::<BTreeSet<_>>();
    vm_cfg.clear_pass_through_devices();
    let reserved_regions: Vec<VmMemConfig> = reserved_memory_regions(crate_cfg).cloned().collect();

    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let node_path = fdt.path_of(node_id);

        if !selected_paths.contains(&node_path)
            || node_path == "/"
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

        let node_name = node.name();
        if node_name.starts_with("pcie@") || node_name.contains("pci") {
            for (index, range) in node_pci_ranges(&fdt, node_id).iter().enumerate() {
                add_pci_ranges_config(vm_cfg, &node_path, range, index);
            }

            for (index, reg) in node_regs(&fdt, node_id).iter().enumerate() {
                add_device_address_config(
                    vm_cfg,
                    &node_path,
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
                    &node_path,
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

pub fn parse_vm_interrupt(
    vm_cfg: &mut AxVMConfig,
    crate_cfg: &GuestConfig,
    dtb: &[u8],
) -> AxVmResult {
    let decode_interrupt = super::selected_guest_fdt_policy().decode_interrupt;
    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading interrupts: {e:#?}")
        )
    })?;
    let selected_paths = super::device::find_all_passthrough_devices(vm_cfg, &fdt)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let excluded_paths = excluded_device_paths(vm_cfg, crate_cfg);
    let host_owned_serial_paths = super::serial::host_owned_serial_paths(&fdt);
    let mut passthrough_interrupts = Vec::new();

    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let name = node.name();
        let path = fdt.path_of(node_id);
        if !selected_paths.contains(&path)
            || is_excluded_node_path(&path, &excluded_paths)
            || name.starts_with("memory")
            || name.starts_with("interrupt-controller")
            || name.starts_with("intc")
            || name.starts_with("its")
            || host_owned_serial_paths.contains(&path)
        {
            continue;
        }

        let Some(view) = fdt.view_typed(node_id) else {
            continue;
        };
        for interrupt in view.interrupts() {
            if let Some(interrupt) = decode_interrupt(&interrupt.specifier) {
                if vm_cfg
                    .excluded_passthrough_irq_sources()
                    .contains(&interrupt.source)
                {
                    return Err(AxVmError::invalid_config(format!(
                        "passthrough device {path} shares host-owned interrupt source {:#x}",
                        interrupt.source
                    )));
                }
                passthrough_interrupts.push((path.clone(), interrupt));
            }
        }
    }

    for (path, interrupt) in passthrough_interrupts {
        trace!(
            "node: {path}, passthrough interrupt source: {:#x}, trigger: {:?}",
            interrupt.source, interrupt.trigger
        );
        vm_cfg.add_pass_through_irq(interrupt.source, interrupt.trigger);
    }

    Ok(())
}
pub fn update_provided_fdt(
    provided_dtb: &[u8],
    host_dtb: Option<&[u8]>,
    crate_config: &GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let patch_provided = super::selected_guest_fdt_policy().patch_provided;
    patch_provided(provided_dtb, host_dtb, crate_config)
}

#[cfg(test)]
mod tests {
    use std::{string::ToString, vec, vec::Vec};

    use axvm_types::{AddressSpacePolicy, HostDeviceAssignment, VmMemConfig, VmMemMappingType};
    use axvmconfig::{GuestConfig, GuestDevices, GuestType, PhysicalDeviceRef};
    use fdt_edit::{Fdt, Node};
    use fdt_raw::RegInfo;

    use super::{
        align_reserved_region_4k, parse_passthrough_devices_address, parse_vm_interrupt,
        reserve_excluded_device_ranges, resolve_phys_cpu_sets, setup_guest_fdt_from_vmm,
    };
    use crate::config::{AxVMConfig, AxVMConfigParams, PhysCpuList};

    fn prop_u32(name: &str, value: u32) -> fdt_edit::Property {
        let mut prop = fdt_edit::Property::new(name, std::vec![]);
        prop.set_u32_ls(&[value]);
        prop
    }

    fn prop_u32_list(name: &str, values: &[u32]) -> fdt_edit::Property {
        let mut prop = fdt_edit::Property::new(name, std::vec![]);
        prop.set_u32_ls(values);
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

    fn fdt_with_pci_host_and_endpoint() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));
        let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(super::super::tree::prop_string("compatible", "riscv,plic0"));
        let soc = fdt.add_node(root, Node::new("soc"));
        let pci = fdt.add_node(soc, Node::new("pci@30000000"));
        fdt.node_mut(pci)
            .unwrap()
            .set_property(super::super::tree::prop_string("device_type", "pci"));
        fdt.view_typed_mut(pci)
            .unwrap()
            .set_regs(&[RegInfo::new(0x3000_0000, Some(0x1000_0000))]);
        let nvme = fdt.add_node(pci, Node::new("nvme@0"));
        fdt.node_mut(nvme)
            .unwrap()
            .set_property(prop_u32("interrupt-parent", 1));
        fdt.node_mut(nvme)
            .unwrap()
            .set_property(prop_u32_list("interrupts", &[11]));
        let virtio = fdt.add_node(soc, Node::new("virtio_mmio@10001000"));
        fdt.node_mut(virtio)
            .unwrap()
            .set_property(super::super::tree::prop_string("compatible", "virtio,mmio"));
        fdt.node_mut(virtio)
            .unwrap()
            .set_property(prop_u32("interrupt-parent", 1));
        fdt.node_mut(virtio)
            .unwrap()
            .set_property(prop_u32_list("interrupts", &[11]));
        fdt.view_typed_mut(virtio)
            .unwrap()
            .set_regs(&[RegInfo::new(0x1000_1000, Some(0x1000))]);
        fdt.encode().as_ref().to_vec()
    }

    fn fdt_with_serial_and_device_interrupts() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 1));

        for (name, compatible, irq) in [
            ("serial@10000000", "ns16550a", 10),
            ("virtio_mmio@10001000", "virtio,mmio", 11),
        ] {
            let node = fdt.add_node(root, Node::new(name));
            fdt.node_mut(node)
                .unwrap()
                .set_property(super::super::tree::prop_string("compatible", compatible));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32("interrupt-parent", 1));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32_list("interrupts", &[irq]));
        }

        fdt.encode().as_ref().to_vec()
    }

    fn fdt_with_console_and_assignable_serial() -> Vec<u8> {
        fdt_with_console_and_assignable_serial_irq(11)
    }

    fn fdt_with_console_and_assignable_serial_irq(assignable_irq: u32) -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 1));

        for (name, base, irq) in [
            ("serial@10000000", 0x1000_0000, 10),
            ("serial@10001000", 0x1000_1000, assignable_irq),
        ] {
            let node = fdt.add_node(root, Node::new(name));
            fdt.node_mut(node)
                .unwrap()
                .set_property(super::super::tree::prop_string("compatible", "ns16550a"));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32("interrupt-parent", 1));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32_list("interrupts", &[irq]));
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x100))]);
        }

        let chosen = fdt.add_node(root, Node::new("chosen"));
        fdt.node_mut(chosen)
            .unwrap()
            .set_property(super::super::tree::prop_string(
                "stdout-path",
                "/serial@10000000:115200",
            ));
        fdt.encode().as_ref().to_vec()
    }

    fn fdt_with_nested_console_and_assignable_serial() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 1));

        let console_bus = fdt.add_node(root, Node::new("console-bus"));
        let peripheral_bus = fdt.add_node(root, Node::new("peripherals"));
        let lookalike_bus = fdt.add_node(root, Node::new("peripherals-extra"));
        for bus in [console_bus, peripheral_bus, lookalike_bus] {
            fdt.node_mut(bus)
                .unwrap()
                .set_property(prop_u32("#address-cells", 2));
            fdt.node_mut(bus)
                .unwrap()
                .set_property(prop_u32("#size-cells", 2));
        }

        for (parent, name, base, irq) in [
            (console_bus, "serial@10000000", 0x1000_0000, 10),
            (peripheral_bus, "serial@10001000", 0x1000_1000, 11),
            (lookalike_bus, "serial@10002000", 0x1000_2000, 12),
        ] {
            let node = fdt.add_node(parent, Node::new(name));
            fdt.node_mut(node)
                .unwrap()
                .set_property(super::super::tree::prop_string("compatible", "ns16550a"));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32("interrupt-parent", 1));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32_list("interrupts", &[irq]));
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x100))]);
        }

        let chosen = fdt.add_node(root, Node::new("chosen"));
        fdt.node_mut(chosen)
            .unwrap()
            .set_property(super::super::tree::prop_string(
                "stdout-path",
                "/console-bus/serial@10000000:115200",
            ));
        fdt.encode().as_ref().to_vec()
    }

    fn fdt_with_platform_timer_and_device_interrupts() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 1));

        for (name, compatible, base, irq) in [
            ("timer@10002000", "vendor,soc-timer", 0x1000_2000, 12),
            ("virtio_mmio@10001000", "virtio,mmio", 0x1000_1000, 11),
            ("gpio@10003000", "vendor,gpio", 0x1000_3000, 13),
        ] {
            let node = fdt.add_node(root, Node::new(name));
            fdt.node_mut(node)
                .unwrap()
                .set_property(super::super::tree::prop_string("compatible", compatible));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32("interrupt-parent", 1));
            fdt.node_mut(node)
                .unwrap()
                .set_property(prop_u32_list("interrupts", &[irq]));
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x1000))]);
        }

        fdt.encode().as_ref().to_vec()
    }

    fn fdt_with_selectable_and_machine_owned_devices() -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let intc = fdt.add_node(root, Node::new("interrupt-controller@c000000"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(super::super::tree::prop_string("compatible", "riscv,plic0"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(fdt_edit::Property::new("interrupt-controller", std::vec![]));
        fdt.view_typed_mut(intc)
            .unwrap()
            .set_regs(&[RegInfo::new(0x0c00_0000, Some(0x40_0000))]);

        for (name, base) in [
            ("ethernet@10001000", 0x1000_1000),
            ("gpio@10002000", 0x1000_2000),
        ] {
            let node = fdt.add_node(root, Node::new(name));
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x1000))]);
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
    fn phys_cpu_set_uses_runtime_logical_index_instead_of_fdt_order() {
        let cpu_nodes = [(0, 0), (1, 1), (2, 2), (3, 3)];
        let runtime_indices_by_hardware_id = [1, 2, 3, 0];

        let (cpu_sets, guest_cpu_ids) =
            resolve_phys_cpu_sets(&[0], &cpu_nodes, 4, |hardware_cpu_id| {
                runtime_indices_by_hardware_id.get(hardware_cpu_id).copied()
            })
            .unwrap();

        assert_eq!(cpu_sets, vec![0b0010]);
        assert_eq!(guest_cpu_ids, vec![0]);
    }

    #[test]
    fn phys_cpu_set_rejects_cpu_missing_from_runtime_topology() {
        let error = resolve_phys_cpu_sets(&[3], &[(3, 3)], 4, |_| None).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("hardware CPU ID 0x3 is missing from the runtime topology")
        );
    }

    #[test]
    fn phys_cpu_set_rejects_logical_index_outside_affinity_mask() {
        let error =
            resolve_phys_cpu_sets(&[3], &[(3, 3)], usize::MAX, |_| Some(usize::BITS as usize))
                .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not fit the host CPU affinity mask")
        );
    }

    #[test]
    fn phys_cpu_set_rejects_logical_index_outside_usable_host_cpus() {
        let error = resolve_phys_cpu_sets(&[3], &[(3, 3)], 4, |_| Some(4)).unwrap_err();

        assert!(error.to_string().contains("outside the 4 usable host CPUs"));
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
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                disabled: vec![PhysicalDeviceRef {
                    path: "/serial@10001234".to_string(),
                }],
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

    #[test]
    fn physical_uart_is_reserved_without_user_exclusion() {
        let dtb = fdt_with_excluded_devices();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            ..Default::default()
        });

        reserve_excluded_device_ranges(&mut vm_cfg, &GuestConfig::default(), &dtb).unwrap();

        assert!(
            vm_cfg
                .excluded_devices()
                .iter()
                .flatten()
                .any(|path| path == "/serial@10001234")
        );
        assert!(
            vm_cfg
                .reserved_address_ranges()
                .iter()
                .any(|range| range.base_gpa == 0x1000_1000 && range.length == 0x1000)
        );
    }

    #[test]
    fn implicit_root_passthrough_excludes_unassigned_pci_resources() {
        let dtb = fdt_with_pci_host_and_endpoint();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, Some(vec![0]), None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(vec![0]),
                guest_type: GuestType::Passthrough,
                ..Default::default()
            },
            ..Default::default()
        };

        let guest_dtb = setup_guest_fdt_from_vmm(&dtb, &mut vm_cfg, &crate_cfg).unwrap();
        let guest = Fdt::from_bytes(&guest_dtb).unwrap();

        assert!(guest.get_by_path_id("/soc/pci@30000000").is_none());
        assert!(guest.get_by_path_id("/soc/pci@30000000/nvme@0").is_none());
        assert!(guest.get_by_path_id("/soc/virtio_mmio@10001000").is_some());
        assert!(
            vm_cfg
                .reserved_address_ranges()
                .iter()
                .any(|range| { range.base_gpa == 0x3000_0000 && range.length == 0x1000_0000 })
        );
        assert!(vm_cfg.excluded_passthrough_irq_sources().contains(&11));

        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &guest_dtb).unwrap();
        let error = parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &guest_dtb).unwrap_err();
        assert_eq!(
            error,
            crate::AxVmError::InvalidConfig {
                detail: "passthrough device /soc/virtio_mmio@10001000 shares host-owned interrupt \
                         source 0xb"
                    .to_string(),
            }
        );
    }

    #[test]
    fn explicit_pci_passthrough_is_published() {
        let dtb = fdt_with_pci_host_and_endpoint();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, Some(vec![0]), None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/soc/pci@30000000".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(vec![0]),
                guest_type: GuestType::Passthrough,
                ..Default::default()
            },
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/soc/pci@30000000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let guest_dtb = setup_guest_fdt_from_vmm(&dtb, &mut vm_cfg, &crate_cfg).unwrap();
        let guest = Fdt::from_bytes(&guest_dtb).unwrap();

        assert!(guest.get_by_path_id("/soc/pci@30000000").is_some());
        assert!(guest.get_by_path_id("/soc/pci@30000000/nvme@0").is_some());
    }

    #[test]
    fn explicitly_selected_non_console_uart_receives_mmio_and_interrupt() {
        let dtb = fdt_with_console_and_assignable_serial();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/serial@10001000".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/serial@10001000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert_eq!(vm_cfg.pass_through_devices().len(), 1);
        let uart = &vm_cfg.pass_through_devices()[0];
        assert_eq!(uart.name, "/serial@10001000");
        assert_eq!(uart.base_gpa, 0x1000_1000);
        assert_eq!(uart.base_hpa, 0x1000_1000);
        assert_eq!(uart.length, 0x100);
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .any(|interrupt| interrupt.source == 11)
        );
        assert!(
            vm_cfg
                .excluded_devices()
                .iter()
                .flatten()
                .all(|path| path != "/serial@10001000")
        );
    }

    #[test]
    fn selected_uart_sharing_host_owned_irq_is_rejected() {
        let dtb = fdt_with_console_and_assignable_serial_irq(10);
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/serial@10001000".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/serial@10001000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        let error = parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap_err();
        let expected_detail =
            "passthrough device /serial@10001000 shares host-owned interrupt source 0xa";

        assert_eq!(
            error,
            crate::AxVmError::InvalidConfig {
                detail: expected_detail.to_string(),
            }
        );
        assert!(vm_cfg.pass_through_irqs().is_empty());
    }

    #[test]
    fn explicitly_selected_console_uart_remains_host_owned() {
        let dtb = fdt_with_console_and_assignable_serial();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/serial@10000000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let error = reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap_err();

        assert_eq!(
            error,
            crate::AxVmError::HostOwnedDevice {
                path: "/serial@10000000".to_string(),
            }
        );
    }

    #[test]
    fn parent_selector_assigns_non_console_uart_descendant_only() {
        let dtb = fdt_with_nested_console_and_assignable_serial();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/peripherals".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/peripherals".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert_eq!(vm_cfg.pass_through_devices().len(), 1);
        let uart = &vm_cfg.pass_through_devices()[0];
        assert_eq!(uart.name, "/peripherals/serial@10001000");
        assert_eq!(uart.base_gpa, 0x1000_1000);
        assert_eq!(uart.base_hpa, 0x1000_1000);
        assert_eq!(uart.length, 0x100);
        assert_eq!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .map(|interrupt| interrupt.source)
                .collect::<Vec<_>>(),
            [11]
        );

        let mut generated_vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/peripherals".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut generated_crate_cfg = crate_cfg.clone();
        generated_crate_cfg.base.phys_cpu_ids = Some(vec![]);
        let guest_dtb =
            setup_guest_fdt_from_vmm(&dtb, &mut generated_vm_cfg, &generated_crate_cfg).unwrap();
        let guest_fdt = Fdt::from_bytes(&guest_dtb).unwrap();
        assert!(
            guest_fdt
                .get_by_path("/peripherals/serial@10001000")
                .is_some()
        );
        assert!(
            guest_fdt
                .get_by_path("/console-bus/serial@10000000")
                .is_none()
        );
        assert!(
            guest_fdt
                .get_by_path("/peripherals-extra/serial@10002000")
                .is_none()
        );
    }

    #[test]
    fn parent_selector_covering_console_uart_is_rejected() {
        let dtb = fdt_with_nested_console_and_assignable_serial();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 0,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/console-bus".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/console-bus".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let error = reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap_err();

        assert_eq!(
            error,
            crate::AxVmError::HostOwnedDevice {
                path: "/console-bus".to_string(),
            }
        );
    }

    #[test]
    fn virtualized_guest_maps_only_the_explicit_physical_device() {
        let dtb = fdt_with_selectable_and_machine_owned_devices();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 2,
            name: "virtualized".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/ethernet@10001000".to_string(),
                ..Default::default()
            }],
            address_space_policy: AddressSpacePolicy::Virtualized,
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/ethernet@10001000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert_eq!(vm_cfg.pass_through_devices().len(), 1);
        assert_eq!(vm_cfg.pass_through_devices()[0].base_gpa, 0x1000_1000);
    }

    #[test]
    fn passthrough_guest_reserves_the_machine_interrupt_controller() {
        let dtb = fdt_with_selectable_and_machine_owned_devices();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 3,
            name: "passthrough".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/".to_string(),
                ..Default::default()
            }],
            address_space_policy: AddressSpacePolicy::Passthrough,
            ..Default::default()
        });
        let mut crate_cfg = GuestConfig::default();
        crate_cfg.base.guest_type = GuestType::Passthrough;

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert!(
            vm_cfg
                .pass_through_devices()
                .iter()
                .all(|device| device.base_gpa != 0x0c00_0000)
        );
        assert!(
            vm_cfg
                .reserved_address_ranges()
                .iter()
                .any(|range| range.base_gpa == 0x0c00_0000 && range.length == 0x40_0000)
        );
    }

    #[test]
    fn passthrough_guest_reserves_platform_timers_owned_by_the_machine() {
        let dtb = fdt_with_platform_timer_and_device_interrupts();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 4,
            name: "passthrough".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/".to_string(),
                ..Default::default()
            }],
            address_space_policy: AddressSpacePolicy::Passthrough,
            ..Default::default()
        });
        let mut crate_cfg = GuestConfig::default();
        crate_cfg.base.guest_type = GuestType::Passthrough;

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_passthrough_devices_address(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert!(
            vm_cfg
                .excluded_devices()
                .iter()
                .flatten()
                .any(|path| path == "/timer@10002000")
        );
        assert!(
            vm_cfg
                .reserved_address_ranges()
                .iter()
                .any(|range| range.base_gpa == 0x1000_2000 && range.length == 0x1000)
        );
        assert!(
            vm_cfg
                .pass_through_devices()
                .iter()
                .all(|device| device.base_gpa != 0x1000_2000)
        );
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .all(|interrupt| interrupt.source != 12)
        );
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .any(|interrupt| interrupt.source == 11)
        );
    }

    #[test]
    fn physical_uart_interrupt_is_not_added_to_passthrough_routes() {
        let dtb = fdt_with_serial_and_device_interrupts();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: "test".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/virtio_mmio@10001000".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let crate_cfg = GuestConfig {
            devices: GuestDevices {
                passthrough: vec![PhysicalDeviceRef {
                    path: "/virtio_mmio@10001000".to_string(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert_eq!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .map(|interrupt| interrupt.source)
                .collect::<Vec<_>>(),
            [11]
        );
    }

    #[test]
    fn excluded_passthrough_device_interrupt_is_not_added_to_routes() {
        let dtb = fdt_with_platform_timer_and_device_interrupts();
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: "passthrough".to_string(),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: vec![HostDeviceAssignment {
                name: "/".to_string(),
                ..Default::default()
            }],
            excluded_devices: vec![vec!["/virtio_mmio@10001000".to_string()]],
            address_space_policy: AddressSpacePolicy::Passthrough,
            ..Default::default()
        });
        let crate_cfg = GuestConfig::default();

        reserve_excluded_device_ranges(&mut vm_cfg, &crate_cfg, &dtb).unwrap();
        parse_vm_interrupt(&mut vm_cfg, &crate_cfg, &dtb).unwrap();

        assert!(vm_cfg.excluded_passthrough_irq_sources().contains(&11));
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .all(|interrupt| interrupt.source != 11)
        );
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .all(|interrupt| interrupt.source != 12)
        );
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .any(|interrupt| interrupt.source == 13)
        );
    }
}
