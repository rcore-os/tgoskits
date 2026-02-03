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
use crate::{AxVMCrateConfig, VMBaseConfig, VMDevicesConfig, VMKernelConfig};

/// Configuration parameters for generating a VM template.
///
/// Groups all parameters needed for VM configuration template generation
/// into a single structure to avoid functions with too many arguments.
pub struct VmTemplateParams {
    /// Unique identifier for the VM
    pub id: usize,
    /// Human-readable name for the VM
    pub name: String,
    /// Type of VM (0=HostVM, 1=RTOS, 2=Linux)
    pub vm_type: usize,
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
/// * `AxVMCrateConfig` - Complete VM configuration structure
pub fn get_vm_config_template(params: VmTemplateParams) -> AxVMCrateConfig {
    AxVMCrateConfig {
        // Basic VM configuration
        base: VMBaseConfig {
            id: params.id,
            name: params.name,
            vm_type: params.vm_type,
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
            bios_path: None, // BIOS not used in most configurations
            bios_load_addr: None,
            dtb_path: None, // Device tree not specified by default
            dtb_load_addr: None,
            ramdisk_path: None, // No initial ramdisk by default
            ramdisk_load_addr: None,
            image_location: Some(params.image_location),
            cmdline: params.cmdline, // Optional kernel command line
            disk_path: None,         // No disk image by default
            memory_regions: vec![],  // Memory regions to be defined per architecture
        },
        // Device configuration - starts empty, can be customized
        devices: VMDevicesConfig {
            emu_devices: vec![],                // No emulated devices by default
            passthrough_devices: vec![],        // No passthrough devices by default
            interrupt_mode: Default::default(), // Use default interrupt mode
        },
    }
}
