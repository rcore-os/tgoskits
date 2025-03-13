use crate::{AxVMCrateConfig, VMBaseConfig, VMDevicesConfig, VMKernelConfig};

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
        base: VMBaseConfig {
            id,
            name,
            vm_type,
            cpu_num,
            phys_cpu_ids: Some((0..cpu_num).into_iter().collect()),
            phys_cpu_sets: None,
        },
        kernel: VMKernelConfig {
            entry_point,
            kernel_path,
            kernel_load_addr,
            bios_path: None,
            bios_load_addr: None,
            dtb_path: None,
            dtb_load_addr: None,
            ramdisk_path: None,
            ramdisk_load_addr: None,
            image_location: Some(image_location),
            cmdline,
            disk_path: None,
            memory_regions: vec![],
        },
        devices: VMDevicesConfig {
            emu_devices: vec![],
            passthrough_devices: vec![],
        },
    }
}
