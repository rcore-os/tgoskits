//! [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/arceos-umhv) [VM](https://github.com/arceos-hypervisor/axvm) config module.
//! [`AxVMCrateConfig`]: the configuration structure for the VM.
//! It is generated from toml file, and then converted to `AxVMConfig` for the VM creation.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[macro_use]
extern crate log;

use alloc::string::String;
use alloc::vec::Vec;

use axerrno::AxResult;

/// A part of `AxVMConfig`, which represents guest VM type.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum VMType {
    /// Host VM, used for boot from Linux like Jailhouse do, named "type1.5".
    VMTHostVM = 0,
    /// Guest RTOS, generally a simple guest OS with most of the resource passthrough.
    #[default]
    VMTRTOS = 1,
    /// Guest Linux, generally a full-featured guest OS with complicated device emulation requirements.
    VMTLinux = 2,
}

impl From<usize> for VMType {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::VMTHostVM,
            1 => Self::VMTRTOS,
            2 => Self::VMTLinux,
            _ => {
                warn!("Unknown VmType value: {}, default to VMTRTOS", value);
                Self::default()
            }
        }
    }
}

impl From<VMType> for usize {
    fn from(value: VMType) -> Self {
        value as usize
    }
}

/// The type of memory mapping.
#[derive(Debug, Clone, PartialEq, Eq, serde_repr::Serialize_repr, serde_repr::Deserialize_repr)]
#[repr(u8)]
pub enum VmMemMappingType {
    /// The memory region is allocated by the VM monitor.
    MapAlloc = 0,
    /// The memory region is identical to the host physical memory region.
    MapIentical = 1,
}

/// The default value of `VmMemMappingType` is `MapAlloc`.
impl Default for VmMemMappingType {
    fn default() -> Self {
        Self::MapAlloc
    }
}

/// A part of `AxVMConfig`, which represents a memory region.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmMemConfig {
    /// The start address of the memory region in GPA.
    pub gpa: usize,
    /// The size of the memory region.
    pub size: usize,
    /// The mappings flags of the memory region, refers to `MappingFlags` provided by `axaddrspace`.
    pub flags: usize,
    /// The type of memory mapping.
    pub map_type: VmMemMappingType,
}

/// A part of `AxVMConfig`, which represents the configuration of an emulated device for a virtual machine.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmulatedDeviceConfig {
    /// The name of the device.
    pub name: String,
    /// The base GPA (Guest Physical Address) of the device.
    pub base_gpa: usize,
    /// The address length of the device.
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    pub irq_id: usize,
    /// The type of emulated device.
    pub emu_type: usize,
    /// The config_list of the device
    pub cfg_list: Vec<usize>,
}

/// A part of `AxVMConfig`, which represents the configuration of a pass-through device for a virtual machine.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PassThroughDeviceConfig {
    /// The name of the device.
    pub name: String,
    /// The base GPA (Guest Physical Address) of the device.
    pub base_gpa: usize,
    /// The base HPA (Host Physical Address) of the device.
    pub base_hpa: usize,
    /// The address length of the device.
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    pub irq_id: usize,
}

/// The configuration structure for the guest VM base info.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMBaseConfig {
    /// VM ID.
    pub id: usize,
    /// VM name.
    pub name: String,
    /// VM type.
    pub vm_type: usize,
    // Resources.
    /// The number of virtual CPUs.
    pub cpu_num: usize,
    /// The physical CPU ids.
    /// - if `None`, vcpu's physical id will be set as vcpu id.
    /// - if set, each vcpu will be assigned to the specified physical CPU mask.
    ///
    /// Some ARM platforms will provide a specified cpu hw id in the device tree, which is
    /// read from `MPIDR_EL1` register (probably for clustering).
    pub phys_cpu_ids: Option<Vec<usize>>,
    /// The mask of physical CPUs who can run this VM.
    ///
    /// - If `None`, vcpu will be scheduled on available physical CPUs randomly.
    /// - If set, each vcpu will be scheduled on the specified physical CPUs.
    ///      
    ///     For example, [0x0101, 0x0010] means:
    ///          - vCpu0 can be scheduled at pCpu0 and pCpu2;
    ///          - vCpu1 will only be scheduled at pCpu1;
    ///      It will phrase an error if the number of vCpus is not equal to the length of `phys_cpu_sets` array.
    pub phys_cpu_sets: Option<Vec<usize>>,
}

/// The configuration structure for the guest VM kernel.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMKernelConfig {
    /// The entry point of the kernel image.
    pub entry_point: usize,
    /// The file path of the kernel image.
    pub kernel_path: String,
    /// The load address of the kernel image.
    pub kernel_load_addr: usize,
    /// The file path of the BIOS image, `None` if not used.
    pub bios_path: Option<String>,
    /// The load address of the BIOS image, `None` if not used.
    pub bios_load_addr: Option<usize>,
    /// The file path of the device tree blob (DTB), `None` if not used.
    pub dtb_path: Option<String>,
    /// The load address of the device tree blob (DTB), `None` if not used.
    pub dtb_load_addr: Option<usize>,
    /// The file path of the ramdisk image, `None` if not used.
    pub ramdisk_path: Option<String>,
    /// The load address of the ramdisk image, `None` if not used.
    pub ramdisk_load_addr: Option<usize>,
    /// The location of the image, default is 'fs'.
    pub image_location: Option<String>,
    /// The command line of the kernel.
    pub cmdline: Option<String>,
    /// The path of the disk image.
    pub disk_path: Option<String>,
    /// Memory Information
    pub memory_regions: Vec<VmMemConfig>,
}

/// The configuration structure for the guest VM devices.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMDevicesConfig {
    /// Emu device Information
    pub emu_devices: Vec<EmulatedDeviceConfig>,
    /// Passthrough device Information
    pub passthrough_devices: Vec<PassThroughDeviceConfig>,
}

/// The configuration structure for the guest VM serialized from a toml file provided by user,
/// and then converted to `AxVMConfig` for the VM creation.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AxVMCrateConfig {
    /// The base configuration for the VM.
    pub base: VMBaseConfig,
    /// The kernel configuration for the VM.
    pub kernel: VMKernelConfig,
    /// The devices configuration for the VM.
    pub devices: VMDevicesConfig,
}

impl AxVMCrateConfig {
    /// Deserialize the toml string to `AxVMCrateConfig`.
    pub fn from_toml(raw_cfg_str: &str) -> AxResult<Self> {
        let config: AxVMCrateConfig = toml::from_str(raw_cfg_str).map_err(|err| {
            warn!("Config TOML parse error {:?}", err.message());
            axerrno::ax_err_type!(InvalidInput, alloc::format!("Error details {err:?}"))
        })?;
        Ok(config)
    }
}
