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

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use core::ptr::NonNull;

use ax_errno::{AxError, AxResult, ax_err_type};
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_memory_addr::MemoryAddr;
use fdt_parser::{Fdt, Node};

use super::vm_fdt::{FdtWriter, FdtWriterNode};
use crate::images::load_vm_image_from_memory;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use axvm::AxVMRef;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
use axvm::GuestPhysAddr;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
use axvm::VMMemoryRegion;
use axvm::config::AxVMCrateConfig;

// use crate::fdt::print::{print_fdt, print_guest_fdt};

fn fdt_write_err(err: impl core::fmt::Display) -> AxError {
    ax_err_type!(InvalidData, format!("Failed to write guest FDT: {err}"))
}

fn should_skip_guest_cpu_prop(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "riscv,cbop-block-size" | "riscv,cboz-block-size" | "riscv,cbom-block-size"
    )
}

/// Generate guest FDT and return DTB data
///
/// # Parameters
/// * `fdt` - Source FDT data
/// * `passthrough_device_names` - Passthrough device name list
/// * `crate_config` - VM creation configuration
///
/// # Return Value
/// Returns the generated DTB data
pub fn crate_guest_fdt(
    fdt: &Fdt,
    passthrough_device_names: &[String],
    crate_config: &AxVMCrateConfig,
) -> AxResult<Vec<u8>> {
    let mut fdt_writer = FdtWriter::new().map_err(fdt_write_err)?;
    // Track the level of the previously processed node for level change handling
    let mut previous_node_level = 0;
    // Maintain a stack of FDT nodes to correctly start and end nodes
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;

    let all_nodes: Vec<Node> = fdt.all_nodes().collect();
    let all_paths = super::build_all_node_paths(&all_nodes);

    for (index, node) in all_nodes.iter().enumerate() {
        let node_path = &all_paths[index];
        let node_action = determine_node_action(node, node_path, passthrough_device_names);

        match node_action {
            NodeAction::RootNode => {
                node_stack.push(fdt_writer.begin_node("").map_err(fdt_write_err)?);
            }
            NodeAction::CpuNode => {
                let need = need_cpu_node(phys_cpu_ids, node, node_path);
                if need {
                    handle_node_level_change(
                        &mut fdt_writer,
                        &mut node_stack,
                        node.level,
                        previous_node_level,
                    )?;
                    node_stack.push(fdt_writer.begin_node(node.name()).map_err(fdt_write_err)?);
                } else {
                    continue;
                }
            }
            NodeAction::Skip => {
                continue;
            }
            _ => {
                trace!(
                    "Found exact passthrough device node: {}, path: {}",
                    node.name(),
                    node_path
                );
                handle_node_level_change(
                    &mut fdt_writer,
                    &mut node_stack,
                    node.level,
                    previous_node_level,
                )?;
                node_stack.push(fdt_writer.begin_node(node.name()).map_err(fdt_write_err)?);
            }
        }

        previous_node_level = node.level;

        // Copy all properties of the node
        for prop in node.propertys() {
            if node_path.starts_with("/cpus") && should_skip_guest_cpu_prop(prop.name) {
                continue;
            }
            fdt_writer
                .property(prop.name, prop.raw_value())
                .map_err(fdt_write_err)?;
        }
    }

    // End all unclosed nodes
    while let Some(node) = node_stack.pop() {
        previous_node_level = previous_node_level
            .checked_sub(1)
            .ok_or_else(|| ax_err_type!(InvalidData, "Invalid FDT node nesting"))?;
        fdt_writer.end_node(node).map_err(fdt_write_err)?;
    }
    if previous_node_level != 0 {
        return Err(ax_err_type!(InvalidData, "Guest FDT has unbalanced nodes"));
    }

    fdt_writer.finish().map_err(fdt_write_err)
}

/// Node processing action enumeration
enum NodeAction {
    /// Skip node, not included in guest FDT
    Skip,
    /// Root node
    RootNode,
    /// CPU node
    CpuNode,
    /// Include node as passthrough device node
    IncludeAsPassthroughDevice,
    /// Include node as child node of passthrough device
    IncludeAsChildNode,
    /// Include node as ancestor node of passthrough device
    IncludeAsAncestorNode,
}

/// Determine node processing action
fn determine_node_action(
    node: &Node,
    node_path: &str,
    passthrough_device_names: &[String],
) -> NodeAction {
    if node.name() == "/" {
        // Special handling for root node
        NodeAction::RootNode
    } else if node.name().starts_with("memory") {
        // Skip memory nodes, will add them later
        NodeAction::Skip
    } else if node_path.starts_with("/cpus") {
        NodeAction::CpuNode
    } else if passthrough_device_names.contains(&node_path.to_string()) {
        // Fully matched passthrough device node
        NodeAction::IncludeAsPassthroughDevice
    }
    // Check if the node is a descendant of a passthrough device (by path inclusion and level validation)
    else if is_descendant_of_passthrough_device(node_path, node.level, passthrough_device_names) {
        NodeAction::IncludeAsChildNode
    }
    // Check if the node is an ancestor of a passthrough device (by path inclusion and level validation)
    else if is_ancestor_of_passthrough_device(node_path, passthrough_device_names) {
        NodeAction::IncludeAsAncestorNode
    } else {
        NodeAction::Skip
    }
}

/// Determine if node is a descendant of passthrough device
/// When node path contains a path from passthrough_device_names and is longer than it, it is its descendant node
/// Also use node_level as validation condition
fn is_descendant_of_passthrough_device(
    node_path: &str,
    node_level: usize,
    passthrough_device_names: &[String],
) -> bool {
    for passthrough_path in passthrough_device_names {
        // Check if the current node is a descendant of a passthrough device
        if node_path.starts_with(passthrough_path) && node_path.len() > passthrough_path.len() {
            // Ensure it is a true descendant path (separated by /)
            if passthrough_path == "/" || node_path.chars().nth(passthrough_path.len()) == Some('/')
            {
                // Use level relationship for validation: the level of a descendant node should be higher than its parent
                // Note: The level of the root node is 1, its direct child node level is 2, and so on
                let expected_parent_level = passthrough_path.matches('/').count();
                let current_node_level = node_level;

                // If passthrough_path is the root node "/", then its child node level should be 2
                // Otherwise, the child node level should be higher than the parent node level
                if (passthrough_path == "/" && current_node_level >= 2)
                    || (passthrough_path != "/" && current_node_level > expected_parent_level)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Handle node level changes to ensure correct FDT structure
fn handle_node_level_change(
    fdt_writer: &mut FdtWriter,
    node_stack: &mut Vec<FdtWriterNode>,
    current_level: usize,
    previous_level: usize,
) -> AxResult {
    if current_level <= previous_level {
        for _ in current_level..=previous_level {
            if let Some(end_node) = node_stack.pop() {
                fdt_writer.end_node(end_node).map_err(fdt_write_err)?;
            }
        }
    }
    Ok(())
}

/// Determine if node is an ancestor of passthrough device
fn is_ancestor_of_passthrough_device(node_path: &str, passthrough_device_names: &[String]) -> bool {
    for passthrough_path in passthrough_device_names {
        // Check if the current node is an ancestor of a passthrough device
        if passthrough_path.starts_with(node_path) && passthrough_path.len() > node_path.len() {
            // Ensure it is a true ancestor path (separated by /)
            let next_char = passthrough_path.chars().nth(node_path.len()).unwrap_or(' ');
            if next_char == '/' || node_path == "/" {
                return true;
            }
        }
    }
    false
}

/// Determine if CPU node is needed
fn cpu_node_id(node_path: &str) -> Option<usize> {
    node_path
        .strip_prefix("/cpus/cpu@")
        .and_then(|rest| rest.split('/').next())
        .and_then(|id| usize::from_str_radix(id, 16).ok())
}

fn cpu_reg_address(node: &Node) -> Option<usize> {
    node.reg()
        .and_then(|mut reg| reg.next())
        .map(|reg_entry| reg_entry.address as usize)
}

fn need_cpu_node(phys_cpu_ids: &[usize], node: &Node, node_path: &str) -> bool {
    if !node_path.starts_with("/cpus/cpu@") {
        return true;
    }

    if let Some(cpu_id) = cpu_node_id(node_path) {
        return phys_cpu_ids.contains(&cpu_id);
    }

    if let Some(cpu_address) = cpu_reg_address(node) {
        debug!(
            "Checking CPU node {} with address 0x{:x}",
            node.name(),
            cpu_address
        );
        return phys_cpu_ids.contains(&cpu_address);
    }

    false
}

/// Add memory node
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
fn add_memory_node(
    new_memory: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
    new_fdt: &mut FdtWriter,
) -> AxResult {
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

    let mut new_value: Vec<u32> = Vec::new();
    for (mem, _cfg) in new_memory.iter().take(configured_region_count).zip(
        crate_config
            .kernel
            .memory_regions
            .iter()
            .take(configured_region_count),
    ) {
        let gpa = mem.gpa.as_usize() as u64;
        let size = mem.size() as u64;
        new_value.push((gpa >> 32) as u32);
        new_value.push((gpa & 0xFFFFFFFF) as u32);
        new_value.push((size >> 32) as u32);
        new_value.push((size & 0xFFFFFFFF) as u32);
    }
    info!("Adding memory node with value: {new_value:x?}");
    new_fdt
        .property_array_u32("reg", new_value.as_ref())
        .map_err(fdt_write_err)?;
    new_fdt
        .property_string("device_type", "memory")
        .map_err(fdt_write_err)?;
    Ok(())
}

#[cfg(any(target_arch = "aarch64", test))]
fn initrd_range_from_image_config(
    ramdisk: Option<&axvm::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let rd = ramdisk?;
    let start = rd.load_gpa.as_usize() as u64;
    let size = rd.size? as u64;
    Some((start, start + size))
}

#[cfg(any(target_arch = "aarch64", test))]
fn sanitize_bootargs(bootargs: &str) -> String {
    const RAMDISK_BOOTARGS: [&str; 3] = ["root=/dev/ram0", "rdinit=/init", "rootwait"];
    const FSCK_REPAIR_BOOTARG: &str = "fsck.repair=yes";

    let rewritten = bootargs.replace(" ro ", " rw ");
    let tokens = rewritten.split_whitespace().collect::<Vec<_>>();
    let has_fsck_policy = tokens.iter().any(|token| {
        matches!(
            *token,
            "fastboot"
                | "fsck.mode=skip"
                | "forcefsck"
                | "fsck.mode=force"
                | "fsckfix"
                | "fsck.repair=yes"
                | "fsck.repair=no"
        )
    });
    let has_block_root = tokens.iter().any(|token| {
        token.starts_with("root=/dev/")
            || token.starts_with("root=PARTLABEL=")
            || token.starts_with("root=LABEL=")
            || token.starts_with("root=UUID=")
            || token.starts_with("root=PARTUUID=")
    });
    let mut sanitized = Vec::with_capacity(tokens.len());
    let mut index = 0;

    while index < tokens.len() {
        if tokens[index..].starts_with(&RAMDISK_BOOTARGS) {
            index += RAMDISK_BOOTARGS.len();
            continue;
        }

        sanitized.push(tokens[index]);
        index += 1;
    }

    if has_block_root && !has_fsck_policy {
        sanitized.push(FSCK_REPAIR_BOOTARG);
    }

    sanitized.join(" ")
}

#[cfg(target_arch = "aarch64")]
pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxResult {
    let mut new_fdt = FdtWriter::new().map_err(fdt_write_err)?;
    let mut previous_node_level = 0;
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let initrd_range = vm
        .with_config(|config| initrd_range_from_image_config(config.image_config.ramdisk.as_ref()));

    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| ax_err_type!(InvalidData, format!("Failed to parse FDT: {e:#?}")))?;

    for node in fdt.all_nodes() {
        if node.name() == "/" {
            node_stack.push(new_fdt.begin_node("").map_err(fdt_write_err)?);
        } else if node.name().starts_with("memory") {
            // Skip memory nodes, will add them later
            continue;
        } else {
            handle_node_level_change(
                &mut new_fdt,
                &mut node_stack,
                node.level,
                previous_node_level,
            )?;
            // Start new node
            node_stack.push(new_fdt.begin_node(node.name()).map_err(fdt_write_err)?);
        }

        previous_node_level = node.level;

        if node.name() == "chosen" {
            for prop in node.propertys() {
                if prop.name.starts_with("linux,initrd-") {
                    if initrd_range.is_some() {
                        info!(
                            "Skipping property: {}, belonging to node: {}",
                            prop.name,
                            node.name()
                        );
                    } else {
                        new_fdt
                            .property(prop.name, prop.raw_value())
                            .map_err(fdt_write_err)?;
                    }
                } else if prop.name == "bootargs" {
                    let bootargs_str = prop.str();
                    let modified_bootargs = sanitize_bootargs(bootargs_str);

                    if modified_bootargs != bootargs_str {
                        debug!(
                            "Modifying bootargs: {} -> {}",
                            bootargs_str, modified_bootargs
                        );
                    }

                    new_fdt
                        .property_string(prop.name, &modified_bootargs)
                        .map_err(fdt_write_err)?;
                } else {
                    debug!(
                        "Find property: {}, belonging to node: {}",
                        prop.name,
                        node.name()
                    );
                    new_fdt
                        .property(prop.name, prop.raw_value())
                        .map_err(fdt_write_err)?;
                }
            }
            if let Some((initrd_start, initrd_end)) = initrd_range {
                info!(
                    "initrd_start: {:x}, initrd_end: {:x}",
                    initrd_start, initrd_end
                );
                new_fdt
                    .property_u64("linux,initrd-start", initrd_start)
                    .map_err(fdt_write_err)?;
                new_fdt
                    .property_u64("linux,initrd-end", initrd_end)
                    .map_err(fdt_write_err)?;
            }
        } else {
            for prop in node.propertys() {
                new_fdt
                    .property(prop.name, prop.raw_value())
                    .map_err(fdt_write_err)?;
            }
        }
    }

    // End all unclosed nodes, and add memory nodes at appropriate positions
    while let Some(node) = node_stack.pop() {
        previous_node_level = previous_node_level
            .checked_sub(1)
            .ok_or_else(|| ax_err_type!(InvalidData, "Invalid FDT node nesting"))?;
        new_fdt.end_node(node).map_err(fdt_write_err)?;

        // add memory node
        if previous_node_level == 1 {
            let memory_regions = vm.memory_regions();
            let memory_node = new_fdt.begin_node("memory").map_err(fdt_write_err)?;
            add_memory_node(&memory_regions, crate_config, &mut new_fdt)?;
            new_fdt.end_node(memory_node).map_err(fdt_write_err)?;
        }
    }

    if previous_node_level != 0 {
        return Err(ax_err_type!(InvalidData, "Guest FDT has unbalanced nodes"));
    }

    info!("Updating FDT memory successfully");

    let new_fdt_bytes = new_fdt.finish().map_err(fdt_write_err)?;

    // crate::fdt::print::print_guest_fdt(new_fdt_bytes.as_slice());
    let vm_clone = vm.clone();
    let dest_addr = calculate_dtb_load_addr(vm, new_fdt_bytes.len())?;
    debug!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    // Load the updated FDT into VM
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm_clone)?;
    Ok(())
}

#[cfg(target_arch = "riscv64")]
pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxResult {
    // Fix up the cached DTB against the runtime layout before boot.
    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let fdt = Fdt::from_bytes(fdt_bytes).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse cached guest FDT: {e:#?}")
        )
    })?;
    // Keep boot metadata such as /chosen from the host FDT when it is available.
    let host_fdt_bytes = super::try_get_host_fdt();
    let host_fdt = host_fdt_bytes
        .map(|bytes| {
            Fdt::from_bytes(bytes).map_err(|e| {
                ax_err_type!(
                    InvalidData,
                    format!("Failed to parse host FDT while updating guest FDT: {e:#?}")
                )
            })
        })
        .transpose()?;
    let new_fdt_bytes =
        patch_guest_fdt_for_runtime(&fdt, &vm.memory_regions(), crate_config, host_fdt.as_ref())?;
    // Recompute the DTB load address from the runtime memory layout.
    let dest_addr = calculate_dtb_load_addr(vm.clone(), new_fdt_bytes.len())?;

    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm)
}

#[cfg(test)]
mod tests {
    use fdt_parser::Fdt;

    use super::{cpu_node_id, initrd_range_from_image_config, need_cpu_node, sanitize_bootargs};
    use axvm::{GuestPhysAddr, config::RamdiskInfo};

    fn test_fdt(dts: &str) -> Fdt<'static> {
        let mut writer = super::FdtWriter::new().unwrap();
        let root = writer.begin_node("").unwrap();
        let cpus = writer.begin_node("cpus").unwrap();
        writer.property_u32("#address-cells", 2).unwrap();
        writer.property_u32("#size-cells", 0).unwrap();

        for line in dts.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (name, reg) = line.split_once('=').unwrap();
            let node = writer.begin_node(name).unwrap();
            writer.property_u32("device_type", 0).unwrap();
            let reg = usize::from_str_radix(reg, 16).unwrap();
            writer.property_array_u32("reg", &[0, reg as u32]).unwrap();
            writer.end_node(node).unwrap();
        }

        writer.end_node(cpus).unwrap();
        writer.end_node(root).unwrap();
        let bytes = writer.finish().unwrap().leak();
        Fdt::from_bytes(bytes).unwrap()
    }

    #[test]
    fn cpu_node_selection_uses_node_id_when_reg_differs() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let nodes: Vec<_> = fdt.all_nodes().collect();
        let paths = crate::fdt::build_all_node_paths(&nodes);
        let selected: Vec<_> = nodes
            .iter()
            .zip(paths.iter())
            .filter(|(_, path)| path.starts_with("/cpus/cpu@"))
            .filter_map(|(node, path)| need_cpu_node(&[0x100], node, path).then(|| path.clone()))
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
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn calculate_dtb_load_addr(vm: AxVMRef, fdt_size: usize) -> AxResult<GuestPhysAddr> {
    const MB: usize = 1024 * 1024;

    // Get main memory from VM memory regions outside the closure
    let main_memory =
        vm.memory_regions().first().cloned().ok_or_else(|| {
            ax_err_type!(InvalidInput, "VM has no memory region for DTB placement")
        })?;

    let dtb_addr = vm.with_config(|config| {
        let dtb_addr = if let Some(addr) = config.image_config.dtb_load_gpa
            && !main_memory.is_identical()
        {
            // If dtb_load_gpa is already set, use the original value
            addr
        } else {
            // If dtb_load_gpa is None, calculate based on memory size and FDT size
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

#[cfg(target_arch = "riscv64")]
pub(crate) fn patch_guest_fdt_for_runtime(
    fdt: &Fdt,
    memory_regions: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
    host_fdt: Option<&Fdt>,
) -> AxResult<Vec<u8>> {
    let mut new_fdt = FdtWriter::new().map_err(fdt_write_err)?;
    let mut previous_node_level = 0usize;
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let mut has_chosen = false;

    for node in fdt.all_nodes() {
        // Drop the stale /memory node and rebuild it later.
        if node.name().starts_with("memory") {
            continue;
        }

        if node.name() == "chosen" {
            has_chosen = true;
        }

        if node.name() == "/" {
            node_stack.push(new_fdt.begin_node("").map_err(fdt_write_err)?);
        } else {
            handle_node_level_change(
                &mut new_fdt,
                &mut node_stack,
                node.level,
                previous_node_level,
            )?;
            node_stack.push(new_fdt.begin_node(node.name()).map_err(fdt_write_err)?);
        }
        previous_node_level = node.level;

        for prop in node.propertys() {
            new_fdt
                .property(prop.name, prop.raw_value())
                .map_err(fdt_write_err)?;
        }
    }

    // Return to the root before inserting synthetic nodes.
    while node_stack.len() > 1 {
        let node = node_stack
            .pop()
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest FDT node stack is empty"))?;
        new_fdt.end_node(node).map_err(fdt_write_err)?;
    }

    if node_stack.len() != 1 {
        return Err(ax_err_type!(InvalidData, "Guest FDT root node is missing"));
    }

    // Restore /chosen from the host FDT when it is missing.
    if !has_chosen
        && let Some(host_fdt) = host_fdt
        && let Some(chosen_node) = host_fdt.find_nodes("/chosen").next()
    {
        let chosen = new_fdt.begin_node("chosen").map_err(fdt_write_err)?;
        for prop in chosen_node.propertys() {
            new_fdt
                .property(prop.name, prop.raw_value())
                .map_err(fdt_write_err)?;
        }
        new_fdt.end_node(chosen).map_err(fdt_write_err)?;
    }

    // Rebuild /memory from the runtime-visible regions that correspond to the
    // user-configured memory layout.
    let memory_node = new_fdt.begin_node("memory").map_err(fdt_write_err)?;
    add_memory_node(memory_regions, crate_config, &mut new_fdt)?;
    new_fdt.end_node(memory_node).map_err(fdt_write_err)?;

    let root = node_stack
        .pop()
        .ok_or_else(|| ax_err_type!(InvalidData, "Guest FDT root node is missing"))?;
    new_fdt.end_node(root).map_err(fdt_write_err)?;

    new_fdt.finish().map_err(fdt_write_err)
}

#[cfg(target_arch = "aarch64")]
pub fn update_cpu_node(
    fdt: &Fdt,
    host_fdt: &Fdt,
    crate_config: &AxVMCrateConfig,
) -> AxResult<Vec<u8>> {
    let mut new_fdt = FdtWriter::new().map_err(fdt_write_err)?;
    let mut previous_node_level = 0;
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;

    // Collect all nodes from both FDTs
    let fdt_all_nodes: Vec<Node> = fdt.all_nodes().collect();
    let host_fdt_all_nodes: Vec<Node> = host_fdt.all_nodes().collect();
    let fdt_all_paths = super::build_all_node_paths(&fdt_all_nodes);
    let host_fdt_all_paths = super::build_all_node_paths(&host_fdt_all_nodes);

    for (index, node) in fdt_all_nodes.iter().enumerate() {
        let node_path = &fdt_all_paths[index];

        if node.name() == "/" {
            node_stack.push(new_fdt.begin_node("").map_err(fdt_write_err)?);
        } else if node_path.starts_with("/cpus") {
            // Skip CPU nodes from fdt, we'll process them from host_fdt later
            continue;
        } else {
            // For all other nodes, include them from fdt as-is without filtering
            handle_node_level_change(
                &mut new_fdt,
                &mut node_stack,
                node.level,
                previous_node_level,
            )?;
            node_stack.push(new_fdt.begin_node(node.name()).map_err(fdt_write_err)?);
        }

        previous_node_level = node.level;

        // Copy all properties of the node (for non-CPU nodes)
        for prop in node.propertys() {
            new_fdt
                .property(prop.name, prop.raw_value())
                .map_err(fdt_write_err)?;
        }
    }

    // Process all CPU nodes from host_fdt
    for (index, node) in host_fdt_all_nodes.iter().enumerate() {
        let node_path = &host_fdt_all_paths[index];

        if node_path.starts_with("/cpus") {
            // For CPU nodes, apply filtering based on host_fdt nodes
            let need = need_cpu_node(phys_cpu_ids, node, node_path);
            if need {
                handle_node_level_change(
                    &mut new_fdt,
                    &mut node_stack,
                    node.level,
                    previous_node_level,
                )?;
                node_stack.push(new_fdt.begin_node(node.name()).map_err(fdt_write_err)?);

                // Copy properties from host CPU node
                for prop in node.propertys() {
                    if should_skip_guest_cpu_prop(prop.name) {
                        continue;
                    }
                    new_fdt
                        .property(prop.name, prop.raw_value())
                        .map_err(fdt_write_err)?;
                }

                previous_node_level = node.level;
            }
        }
    }

    // End all unclosed nodes
    while let Some(node) = node_stack.pop() {
        previous_node_level = previous_node_level
            .checked_sub(1)
            .ok_or_else(|| ax_err_type!(InvalidData, "Invalid FDT node nesting"))?;
        new_fdt.end_node(node).map_err(fdt_write_err)?;
    }
    if previous_node_level != 0 {
        return Err(ax_err_type!(InvalidData, "Guest FDT has unbalanced nodes"));
    }

    new_fdt.finish().map_err(fdt_write_err)
}
