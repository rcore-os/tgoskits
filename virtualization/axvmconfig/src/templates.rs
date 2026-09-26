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

//! VM configuration template generation module.
//!
//! This module provides functionality to generate VM configuration templates
//! with sensible defaults based on user-provided parameters.
use crate::*;

/// Access bits a guest's own RAM is mapped with: read, write and execute.
///
/// Every configuration in this repository maps guest RAM this way, and a region
/// without them is not one a guest can run from.
const GUEST_RAM_FLAGS: usize = 0x7;

/// Configuration parameters for generating a VM template.
///
/// Groups all parameters needed for VM configuration template generation
/// into a single structure to avoid functions with too many arguments.
pub struct VmTemplateParams {
    /// Unique identifier for the VM
    pub id: usize,
    /// Human-readable name for the VM
    pub name: String,
    /// Physical-device assignment model.
    pub guest_type: GuestType,
    /// Number of virtual CPUs to allocate
    pub cpu_num: usize,
    /// VM entry point address
    pub entry_point: usize,
    /// Path to the kernel image file
    pub kernel_path: String,
    /// Address where kernel should be loaded
    pub kernel_load_addr: usize,
    /// Location of kernel image ("fs" or "memory")
    pub image_location: String,
    /// Optional kernel command line parameters
    pub cmdline: Option<String>,
    /// Guest physical address the guest's memory region starts at.
    ///
    /// A parameter for the same reason the entry point is: the address belongs to
    /// the guest image and the machine it is written for, not to this platform.
    pub memory_base: usize,
    /// Size of that region, in mebibytes.
    pub memory_mb: usize,
}

/// Generate a VM configuration template with specified parameters.
///
/// Creates a complete VM configuration structure with the provided parameters
/// and sensible defaults for optional fields. This is used by the CLI tool
/// to generate TOML configuration files.
///
/// # Arguments
/// * `params` - Template parameters containing all VM configuration settings
///
/// # Returns
/// * `GuestConfig` - Complete VM configuration structure
pub fn get_vm_config_template(params: VmTemplateParams) -> GuestConfig {
    GuestConfig {
        // Basic VM configuration
        base: VMBaseConfig {
            id: params.id,
            name: params.name,
            guest_type: params.guest_type,
            cpu_num: params.cpu_num,
            // Assign sequential CPU IDs starting from 0
            phys_cpu_ids: Some((0..params.cpu_num).collect()),
            phys_cpu_sets: None,
        },
        // Kernel and boot configuration
        kernel: VMKernelConfig {
            entry_point: params.entry_point,
            kernel_path: params.kernel_path,
            kernel_load_addr: params.kernel_load_addr,
            enable_bios: false,
            boot_protocol: None,
            bios_path: None, // BIOS not used in most configurations
            uefi_firmware_path: None,
            bios_load_addr: None,
            dtb_path: None, // Device tree not specified by default
            dtb_load_addr: None,
            ramdisk_path: None, // No initial ramdisk by default
            ramdisk_load_addr: None,
            image_location: Some(params.image_location),
            cmdline: params.cmdline, // Optional kernel command line
            // The guest's own memory, and the only region this builder can know
            // about: without one the guest has no RAM at all, and the creation
            // path refuses a configuration that names no region. Saturating on
            // the unit conversion rather than wrapping, because a size past what
            // the host can hold is refused when the region is allocated, while a
            // wrapped value would name a small region that looks valid.
            memory_regions: vec![VmMemConfig {
                gpa: params.memory_base,
                size: params.memory_mb.saturating_mul(1024 * 1024),
                flags: GUEST_RAM_FLAGS,
                map_type: VmMemMappingType::MapAlloc,
            }],
            // One region came from the caller, so it counts as a configured one.
            // This is the value that parsing this template back from TOML
            // produces, which keeps the two ways of building one configuration
            // identical.
            configured_memory_region_count: 1,
        },
        // Machine-profile devices, including the virtual serial port, are
        // intentionally absent from the user configuration.
        devices: GuestDevices::default(),
    }
}
