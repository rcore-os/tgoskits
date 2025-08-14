//! VM configuration template generation module.
//!
//! This module provides functionality to generate VM configuration templates
//! with sensible defaults based on user-provided parameters.
use crate::{AxVMCrateConfig, VMBaseConfig, VMDevicesConfig, VMKernelConfig};

/// Generate a VM configuration template with specified parameters.
///
/// Creates a complete VM configuration structure with the provided parameters
/// and sensible defaults for optional fields. This is used by the CLI tool
/// to generate TOML configuration files.
///
/// # Arguments
/// * `id` - Unique identifier for the VM
/// * `name` - Human-readable name for the VM
/// * `vm_type` - Type of VM (0=HostVM, 1=RTOS, 2=Linux)
/// * `cpu_num` - Number of virtual CPUs to allocate
/// * `entry_point` - VM entry point address
/// * `kernel_path` - Path to the kernel image file
/// * `kernel_load_addr` - Address where kernel should be loaded
/// * `image_location` - Location of kernel image ("fs" or "memory")
/// * `cmdline` - Optional kernel command line parameters
///
/// # Returns
/// * `AxVMCrateConfig` - Complete VM configuration structure
pub fn get_vm_config_template(
    id: usize,
    name: String,
    vm_type: usize,
    cpu_num: usize,
    entry_point: usize,
    kernel_path: String,
    kernel_load_addr: usize,
    image_location: String,
    cmdline: Option<String>,
) -> AxVMCrateConfig {
    AxVMCrateConfig {
        // Basic VM configuration
        base: VMBaseConfig {
            id,
            name,
            vm_type,
            cpu_num,
            // Assign sequential CPU IDs starting from 0
            phys_cpu_ids: Some((0..cpu_num).into_iter().collect()),
            phys_cpu_sets: None,
        },
        // Kernel and boot configuration
        kernel: VMKernelConfig {
            entry_point,
            kernel_path,
            kernel_load_addr,
            bios_path: None, // BIOS not used in most configurations
            bios_load_addr: None,
            dtb_path: None, // Device tree not specified by default
            dtb_load_addr: None,
            ramdisk_path: None, // No initial ramdisk by default
            ramdisk_load_addr: None,
            image_location: Some(image_location),
            cmdline,                // Optional kernel command line
            disk_path: None,        // No disk image by default
            memory_regions: vec![], // Memory regions to be defined per architecture
        },
        // Device configuration - starts empty, can be customized
        devices: VMDevicesConfig {
            emu_devices: vec![],                // No emulated devices by default
            passthrough_devices: vec![],        // No passthrough devices by default
            interrupt_mode: Default::default(), // Use default interrupt mode
        },
    }
}
