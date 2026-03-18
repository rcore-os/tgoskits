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

use fdt_parser::{Fdt, FdtHeader};

#[allow(dead_code)]
pub fn print_fdt(fdt_addr: usize) {
    const FDT_VALID_MAGIC: u32 = 0xd00d_feed;
    let header = unsafe {
        core::slice::from_raw_parts(fdt_addr as *const u8, core::mem::size_of::<FdtHeader>())
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
        return;
    }

    let fdt_bytes =
        unsafe { core::slice::from_raw_parts(fdt_addr as *const u8, fdt_header.total_size()) };

    let fdt = Fdt::from_bytes(fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");

    // Statistics of node count and level distribution
    let mut node_count = 0;
    let mut level_counts = alloc::collections::BTreeMap::new();
    let mut max_level = 0;

    info!("=== FDT Node Information Statistics ===");

    // Traverse all nodes once for statistics (following optimization strategy)
    for node in fdt.all_nodes() {
        node_count += 1;

        // Count nodes by level
        *level_counts.entry(node.level).or_insert(0) += 1;

        // Record maximum level
        if node.level > max_level {
            max_level = node.level;
        }

        // Count property numbers
        let node_properties_count = node.propertys().count();

        trace!(
            "Node[{}]: {} (Level: {}, Properties: {})",
            node_count,
            node.name(),
            node.level,
            node_properties_count
        );

        for prop in node.propertys() {
            trace!(
                "Properties: {}, Raw_value: {:x?}",
                prop.name,
                prop.raw_value()
            );
        }
    }

    info!("=== FDT Statistics Results ===");
    info!("Total node count: {node_count}");
    info!("FDT total size: {} bytes", fdt_header.total_size());
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
    for node in fdt.all_nodes() {
        node_count += 1;

        // Count nodes by level
        *level_counts.entry(node.level).or_insert(0) += 1;

        // Record maximum level
        if node.level > max_level {
            max_level = node.level;
        }

        // Count property numbers
        let node_properties_count = node.propertys().count();

        info!(
            "Node[{}]: {} (Level: {}, Properties: {})",
            node_count,
            node.name(),
            node.level,
            node_properties_count
        );

        for prop in node.propertys() {
            info!(
                "Properties: {}, Raw_value: {:x?}",
                prop.name,
                prop.raw_value()
            );
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
