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

//! FDT (Flattened Device Tree) processing module for AxVisor.
//!
//! This module provides functionality for parsing and processing device tree blobs,
//! including CPU configuration, passthrough device detection, and FDT generation.

mod create;
mod device;
mod parser;
mod print;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use axvm::config::{AxVMConfig, AxVMCrateConfig};
use fdt_parser::Fdt;
use lazyinit::LazyInit;
use spin::Mutex;

pub use parser::*;
// pub use print::print_fdt;
pub use create::*;
pub use device::build_node_path;

use crate::vmm::config::{config, get_vm_dtb_arc};

// DTB cache for generated device trees
static GENERATED_DTB_CACHE: LazyInit<Mutex<BTreeMap<usize, Vec<u8>>>> = LazyInit::new();

/// Initialize the DTB cache
pub fn init_dtb_cache() {
    GENERATED_DTB_CACHE.init_once(Mutex::new(BTreeMap::new()));
}

/// Get reference to the DTB cache
pub fn dtb_cache() -> &'static Mutex<BTreeMap<usize, Vec<u8>>> {
    GENERATED_DTB_CACHE.get().unwrap()
}

/// Generate guest FDT cache the result
/// # Return Value
/// Returns the generated DTB data and stores it in the global cache
pub fn crate_guest_fdt_with_cache(dtb_data: Vec<u8>, crate_config: &AxVMCrateConfig) {
    // Store data in global cache
    let mut cache_lock = dtb_cache().lock();
    cache_lock.insert(crate_config.base.id, dtb_data);
}

/// Handle all FDT-related operations for aarch64 architecture
pub fn handle_fdt_operations(vm_config: &mut AxVMConfig, vm_create_config: &AxVMCrateConfig) {
    let host_fdt_bytes = get_host_fdt();
    let host_fdt = Fdt::from_bytes(host_fdt_bytes)
        .map_err(|e| format!("Failed to parse FDT: {e:#?}"))
        .expect("Failed to parse FDT");
    set_phys_cpu_sets(vm_config, &host_fdt, vm_create_config);

    if let Some(provided_dtb) = get_developer_provided_dtb(vm_config, vm_create_config) {
        info!("VM[{}] found DTB , parsing...", vm_config.id());
        update_provided_fdt(&provided_dtb, host_fdt_bytes, vm_create_config);
    } else {
        info!(
            "VM[{}] DTB not found, generating based on the configuration file.",
            vm_config.id()
        );
        setup_guest_fdt_from_vmm(host_fdt_bytes, vm_config, vm_create_config);
    }

    // Overlay VM config with the given DTB.
    if let Some(dtb_arc) = get_vm_dtb_arc(vm_config) {
        let dtb = dtb_arc.as_ref();
        parse_passthrough_devices_address(vm_config, dtb);
        parse_vm_interrupt(vm_config, dtb);
    } else {
        error!(
            "VM[{}] DTB not found in memory, skipping...",
            vm_config.id()
        );
    }
}

pub fn get_developer_provided_dtb(
    vm_cfg: &AxVMConfig,
    crate_config: &AxVMCrateConfig,
) -> Option<Vec<u8>> {
    match crate_config.kernel.image_location.as_deref() {
        Some("memory") => {
            let vm_imags = config::get_memory_images()
                .iter()
                .find(|&v| v.id == vm_cfg.id())?;

            if let Some(dtb) = vm_imags.dtb {
                info!("DTB file in memory, size: 0x{:x}", dtb.len());
                return Some(dtb.to_vec());
            }
        }
        #[cfg(feature = "fs")]
        Some("fs") => {
            use axerrno::ax_err_type;
            use std::io::{BufReader, Read};
            if let Some(dtb_path) = &crate_config.kernel.dtb_path {
                let (dtb_file, dtb_size) =
                    crate::vmm::images::fs::open_image_file(dtb_path).unwrap();
                info!("DTB file in fs, size: 0x{:x}", dtb_size);

                let mut file = BufReader::new(dtb_file);
                let mut dtb_buffer = vec![0; dtb_size];

                file.read_exact(&mut dtb_buffer)
                    .map_err(|err| {
                        ax_err_type!(
                            Io,
                            format!("Failed in reading from file {}, err {:?}", dtb_path, err)
                        )
                    })
                    .unwrap();
                return Some(dtb_buffer);
            }
        }
        _ => unimplemented!(
            "Check your \"image_location\" in config.toml, \"memory\" and \"fs\" are supported,\n."
        ),
    }
    None
}
