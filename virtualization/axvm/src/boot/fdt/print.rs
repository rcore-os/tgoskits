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

use alloc::format;

use fdt_edit::Fdt;
use fdt_raw::Header;

#[allow(dead_code)]
pub fn print_fdt(fdt_addr: usize) {
    let header = unsafe {
        core::slice::from_raw_parts(fdt_addr as *const u8, core::mem::size_of::<Header>())
    };
    let fdt_header = Header::from_bytes(header)
        .map_err(|e| format!("Failed to parse FDT header: {e:#?}"))
        .unwrap();

    let fdt_bytes = unsafe {
        core::slice::from_raw_parts(fdt_addr as *const u8, fdt_header.totalsize as usize)
    };

    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");

    // Statistics of node count and level distribution
    let mut node_count = 0;
    let mut level_counts = alloc::collections::BTreeMap::new();
    let mut max_level = 0;

    info!("=== FDT Node Information Statistics ===");

    // Traverse all nodes once for statistics (following optimization strategy)
    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let level = fdt.path_of(node_id).matches('/').count().max(1);
        node_count += 1;

        // Count nodes by level
        *level_counts.entry(level).or_insert(0) += 1;

        // Record maximum level
        if level > max_level {
            max_level = level;
        }

        // Count property numbers
        let node_properties_count = node.properties().len();

        trace!(
            "Node[{}]: {} (Level: {}, Properties: {})",
            node_count,
            node.name(),
            level,
            node_properties_count
        );

        for prop in node.properties() {
            trace!("Properties: {}, Raw_value: {:x?}", prop.name(), prop.data);
        }
    }

    info!("=== FDT Statistics Results ===");
    info!("Total node count: {node_count}");
    info!("FDT total size: {} bytes", fdt_header.totalsize);
    info!("Maximum level depth: {max_level}");

    info!("Node distribution by level:");
    for (level, count) in level_counts {
        let percentage = (count as f32 / node_count as f32) * 100.0;
        info!("  Level {level}: {count} nodes ({percentage:.1}%)");
    }
}

#[allow(dead_code)]
pub fn print_guest_fdt(fdt_bytes: &[u8]) {
    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");
    // Statistics of node count and level distribution
    let mut node_count = 0;
    let mut level_counts = alloc::collections::BTreeMap::new();
    let mut max_level = 0;

    info!("=== FDT Node Information Statistics ===");

    // Traverse all nodes once for statistics (following optimization strategy)
    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let level = fdt.path_of(node_id).matches('/').count().max(1);
        node_count += 1;

        // Count nodes by level
        *level_counts.entry(level).or_insert(0) += 1;

        // Record maximum level
        if level > max_level {
            max_level = level;
        }

        // Count property numbers
        let node_properties_count = node.properties().len();

        info!(
            "Node[{}]: {} (Level: {}, Properties: {})",
            node_count,
            node.name(),
            level,
            node_properties_count
        );

        for prop in node.properties() {
            info!("Properties: {}, Raw_value: {:x?}", prop.name(), prop.data);
        }
    }

    info!("=== FDT Statistics Results ===");
    info!("Total node count: {node_count}");
    info!("Maximum level depth: {max_level}");

    info!("Node distribution by level:");
    for (level, count) in level_counts {
        let percentage = (count as f32 / node_count as f32) * 100.0;
        info!("  Level {level}: {count} nodes ({percentage:.1}%)");
    }
}
