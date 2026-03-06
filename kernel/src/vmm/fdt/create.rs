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
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use axaddrspace::GuestPhysAddr;
use axvm::{VMMemoryRegion, config::AxVMCrateConfig};
use fdt_parser::{Fdt, Node};
use memory_addr::MemoryAddr;
use vm_fdt::{FdtWriter, FdtWriterNode};

use crate::vmm::{VMRef, images::load_vm_image_from_memory};

// use crate::vmm::fdt::print::{print_fdt, print_guest_fdt};
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
) -> Vec<u8> {
    let mut fdt_writer = FdtWriter::new().unwrap();
    // Track the level of the previously processed node for level change handling
    let mut previous_node_level = 0;
    // Maintain a stack of FDT nodes to correctly start and end nodes
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .clone()
        .expect("ERROR: phys_cpu_ids is None");

    let all_nodes: Vec<Node> = fdt.all_nodes().collect();

    for (index, node) in all_nodes.iter().enumerate() {
        let node_path = super::build_node_path(&all_nodes, index);
        let node_action = determine_node_action(node, &node_path, passthrough_device_names);

        match node_action {
            NodeAction::RootNode => {
                node_stack.push(fdt_writer.begin_node("").unwrap());
            }
            NodeAction::CpuNode => {
                let need = need_cpu_node(&phys_cpu_ids, node, &node_path);
                if need {
                    handle_node_level_change(
                        &mut fdt_writer,
                        &mut node_stack,
                        node.level,
                        previous_node_level,
                    );
                    node_stack.push(fdt_writer.begin_node(node.name()).unwrap());
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
                );
                node_stack.push(fdt_writer.begin_node(node.name()).unwrap());
            }
        }

        previous_node_level = node.level;

        // Copy all properties of the node
        for prop in node.propertys() {
            fdt_writer.property(prop.name, prop.raw_value()).unwrap();
        }
    }

    // End all unclosed nodes
    while let Some(node) = node_stack.pop() {
        previous_node_level -= 1;
        fdt_writer.end_node(node).unwrap();
    }
    assert_eq!(previous_node_level, 0);

    fdt_writer.finish().unwrap()
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
) {
    if current_level <= previous_level {
        for _ in current_level..=previous_level {
            if let Some(end_node) = node_stack.pop() {
                fdt_writer.end_node(end_node).unwrap();
            }
        }
    }
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
fn need_cpu_node(phys_cpu_ids: &[usize], node: &Node, node_path: &str) -> bool {
    let mut should_include_node = false;

    if !node_path.starts_with("/cpus/cpu@") {
        should_include_node = true;
    } else if let Some(mut cpu_reg) = node.reg()
        && let Some(reg_entry) = cpu_reg.next()
    {
        let cpu_address = reg_entry.address as usize;
        debug!(
            "Checking CPU node {} with address 0x{:x}",
            node.name(),
            cpu_address
        );
        // Check if this CPU address is in the configured phys_cpu_ids
        if phys_cpu_ids.contains(&cpu_address) {
            should_include_node = true;
            debug!(
                "CPU node {} with address 0x{:x} is in phys_cpu_ids, including in guest FDT",
                node.name(),
                cpu_address
            );
        } else {
            debug!(
                "CPU node {} with address 0x{:x} is NOT in phys_cpu_ids, skipping",
                node.name(),
                cpu_address
            );
        }
    }
    should_include_node
}

/// Add memory node
fn add_memory_node(new_memory: &[VMMemoryRegion], new_fdt: &mut FdtWriter) {
    let mut new_value: Vec<u32> = Vec::new();
    for mem in new_memory {
        let gpa = mem.gpa.as_usize() as u64;
        let size = mem.size() as u64;
        new_value.push((gpa >> 32) as u32);
        new_value.push((gpa & 0xFFFFFFFF) as u32);
        new_value.push((size >> 32) as u32);
        new_value.push((size & 0xFFFFFFFF) as u32);
    }
    info!("Adding memory node with value: 0x{new_value:x?}");
    new_fdt
        .property_array_u32("reg", new_value.as_ref())
        .unwrap();
    new_fdt.property_string("device_type", "memory").unwrap();
}

pub fn update_fdt(fdt_src: NonNull<u8>, dtb_size: usize, vm: VMRef) {
    let mut new_fdt = FdtWriter::new().unwrap();
    let mut previous_node_level = 0;
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();

    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");

    for node in fdt.all_nodes() {
        if node.name() == "/" {
            node_stack.push(new_fdt.begin_node("").unwrap());
        } else if node.name().starts_with("memory") {
            // Skip memory nodes, will add them later
            continue;
        } else {
            handle_node_level_change(
                &mut new_fdt,
                &mut node_stack,
                node.level,
                previous_node_level,
            );
            // Start new node
            node_stack.push(new_fdt.begin_node(node.name()).unwrap());
        }

        previous_node_level = node.level;

        if node.name() == "chosen" {
            for prop in node.propertys() {
                if prop.name.starts_with("linux,initrd-") {
                    info!(
                        "Skipping property: {}, belonging to node: {}",
                        prop.name,
                        node.name()
                    );
                } else if prop.name == "bootargs" {
                    let bootargs_str = prop.str();
                    let modified_bootargs = bootargs_str.replace(" ro ", " rw ");

                    if modified_bootargs != bootargs_str {
                        info!(
                            "Modifying bootargs: {} -> {}",
                            bootargs_str, modified_bootargs
                        );
                    }

                    new_fdt
                        .property_string(prop.name, &modified_bootargs)
                        .unwrap();
                } else {
                    debug!(
                        "Find property: {}, belonging to node: {}",
                        prop.name,
                        node.name()
                    );
                    new_fdt.property(prop.name, prop.raw_value()).unwrap();
                }
            }
        } else {
            for prop in node.propertys() {
                new_fdt.property(prop.name, prop.raw_value()).unwrap();
            }
        }
    }

    // End all unclosed nodes, and add memory nodes at appropriate positions
    while let Some(node) = node_stack.pop() {
        previous_node_level -= 1;
        new_fdt.end_node(node).unwrap();

        // add memory node
        if previous_node_level == 1 {
            let memory_regions = vm.memory_regions();
            debug!("Adding memory node with regions: {memory_regions:?}");
            let memory_node = new_fdt.begin_node("memory").unwrap();
            add_memory_node(&memory_regions, &mut new_fdt);
            new_fdt.end_node(memory_node).unwrap();
        }
    }

    assert_eq!(previous_node_level, 0);

    info!("Updating FDT memory successfully");

    let new_fdt_bytes = new_fdt.finish().unwrap();

    // crate::vmm::fdt::print::print_guest_fdt(new_fdt_bytes.as_slice());
    let vm_clone = vm.clone();
    let dest_addr = calculate_dtb_load_addr(vm, new_fdt_bytes.len());
    info!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    // Load the updated FDT into VM
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm_clone)
        .expect("Failed to load VM images");
}

fn calculate_dtb_load_addr(vm: VMRef, fdt_size: usize) -> GuestPhysAddr {
    const MB: usize = 1024 * 1024;

    // Get main memory from VM memory regions outside the closure
    let main_memory = vm
        .memory_regions()
        .first()
        .cloned()
        .expect("VM must have at least one memory region");

    vm.with_config(|config| {
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
    })
}

pub fn update_cpu_node(fdt: &Fdt, host_fdt: &Fdt, crate_config: &AxVMCrateConfig) -> Vec<u8> {
    let mut new_fdt = FdtWriter::new().unwrap();
    let mut previous_node_level = 0;
    let mut node_stack: Vec<FdtWriterNode> = Vec::new();
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .clone()
        .expect("ERROR: phys_cpu_ids is None");

    // Collect all nodes from both FDTs
    let fdt_all_nodes: Vec<Node> = fdt.all_nodes().collect();
    let host_fdt_all_nodes: Vec<Node> = host_fdt.all_nodes().collect();

    for (index, node) in fdt_all_nodes.iter().enumerate() {
        let node_path = super::build_node_path(&fdt_all_nodes, index);

        if node.name() == "/" {
            node_stack.push(new_fdt.begin_node("").unwrap());
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
            );
            node_stack.push(new_fdt.begin_node(node.name()).unwrap());
        }

        previous_node_level = node.level;

        // Copy all properties of the node (for non-CPU nodes)
        for prop in node.propertys() {
            new_fdt.property(prop.name, prop.raw_value()).unwrap();
        }
    }

    // Process all CPU nodes from host_fdt
    for (index, node) in host_fdt_all_nodes.iter().enumerate() {
        let node_path = super::build_node_path(&host_fdt_all_nodes, index);

        if node_path.starts_with("/cpus") {
            // For CPU nodes, apply filtering based on host_fdt nodes
            let need = need_cpu_node(&phys_cpu_ids, node, &node_path);
            if need {
                handle_node_level_change(
                    &mut new_fdt,
                    &mut node_stack,
                    node.level,
                    previous_node_level,
                );
                node_stack.push(new_fdt.begin_node(node.name()).unwrap());

                // Copy properties from host CPU node
                for prop in node.propertys() {
                    new_fdt.property(prop.name, prop.raw_value()).unwrap();
                }

                previous_node_level = node.level;
            }
        }
    }

    // End all unclosed nodes
    while let Some(node) = node_stack.pop() {
        previous_node_level -= 1;
        new_fdt.end_node(node).unwrap();
    }
    assert_eq!(previous_node_level, 0);

    new_fdt.finish().unwrap()
}
