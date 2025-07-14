//! [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/arceos-umhv)
//! [VM](https://github.com/arceos-hypervisor/axvm) config module.
//! [`AxVMCrateConfig`]: the configuration structure for the VM.
//! It is generated from toml file, and then converted to `AxVMConfig` for the VM creation.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[macro_use]
extern crate log;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{Display, Formatter};
use enumerable::Enumerable;
use serde_repr::{Deserialize_repr, Serialize_repr};

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
    MapIdentical = 1,
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

/// The type of Emulated Device.
///
/// Allocation scheme:
/// - 0x00 - 0x1F: Special devices, and abstract device types that does not specify a concrete
/// interface or implementation. The device objects created from these types depend on the target
/// architecture and the specific implementation of the hypervisor.
/// - 0x20 - 0x7F: Concrete emulated device types.
///   - 0x20 - 0x2F: Interrupt controller devices.
///   - 0x30 - 0x3F: Reserved for future use.
/// - 0x80 - 0xDF: Reserved for future use.
/// - 0xE0 - 0xEF: Virtio devices.
/// - 0xF0 - 0xFF: Reserved for future use.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr, Enumerable)]
#[repr(u8)]
pub enum EmulatedDeviceType {
    // Special devices and abstract device types.
    /// Dummy device type.
    Dummy = 0x0,
    /// Interrupt controller device, e.g. vGICv2 in aarch64, vLAPIC in x86.
    InterruptController = 0x1,
    /// Console (serial) device.
    Console = 0x2,
    /// An emulated device that provides Inter-VM Communication (IVC) channel.
    ///
    /// This device is used for communication between different VMs,
    /// the corresponding memory region of this device should be marked as `Reserved` in
    /// device tree or ACPI table.
    IVCChannel = 0xA,

    // Arch-specific interrupt controller devices.
    // 0x20 - 0x22: GPPT (GIC Partial Passthrough) devices.
    /// ARM GIC Partial Passthrough Redistributor device.
    GPPTRedistributor = 0x20,
    /// ARM GIC Partial Passthrough Distributor device.
    GPPTDistributor = 0x21,
    /// ARM GIC Partial Passthrough Interrupt Translation Service device.
    GPPTITS = 0x22,

    // Virtio devices.
    /// Virtio block device.
    VirtioBlk = 0xE1,
    /// Virtio net device.
    VirtioNet = 0xE2,
    /// Virtio console device.
    VirtioConsole = 0xE3,
    // Following are some other emulated devices that are not currently used and removed from the enum temporarily.
    // /// IOMMU device.
    // IOMMU = 0x6,
    // /// Interrupt ICC SRE device.
    // ICCSRE = 0x7,
    // /// Interrupt ICC SGIR device.
    // SGIR = 0x8,
    // /// Interrupt controller GICR device.
    // GICR = 0x9,
}

impl Default for EmulatedDeviceType {
    fn default() -> Self {
        Self::Dummy
    }
}

impl Display for EmulatedDeviceType {
    // Implementation of the Display trait for EmulatedDeviceType.
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            EmulatedDeviceType::Console => write!(f, "console"),
            EmulatedDeviceType::InterruptController => write!(f, "interrupt controller"),
            EmulatedDeviceType::GPPTRedistributor => {
                write!(f, "gic partial passthrough redistributor")
            }
            EmulatedDeviceType::GPPTDistributor => write!(f, "gic partial passthrough distributor"),
            EmulatedDeviceType::GPPTITS => write!(f, "gic partial passthrough its"),
            // EmulatedDeviceType::IOMMU => write!(f, "iommu"),
            // EmulatedDeviceType::ICCSRE => write!(f, "interrupt icc sre"),
            // EmulatedDeviceType::SGIR => write!(f, "interrupt icc sgir"),
            // EmulatedDeviceType::GICR => write!(f, "interrupt controller gicr"),
            EmulatedDeviceType::IVCChannel => write!(f, "ivc channel"),
            EmulatedDeviceType::Dummy => write!(f, "meta device"),
            EmulatedDeviceType::VirtioBlk => write!(f, "virtio block"),
            EmulatedDeviceType::VirtioNet => write!(f, "virtio net"),
            EmulatedDeviceType::VirtioConsole => write!(f, "virtio console"),
        }
    }
}

/// Implementation of methods for EmulatedDeviceType.
impl EmulatedDeviceType {
    /// Returns true if the device is removable.
    pub fn removable(&self) -> bool {
        matches!(
            *self,
            EmulatedDeviceType::InterruptController
                // | EmulatedDeviceType::SGIR
                // | EmulatedDeviceType::ICCSRE
                | EmulatedDeviceType::GPPTRedistributor
                | EmulatedDeviceType::VirtioBlk
                | EmulatedDeviceType::VirtioNet
                // | EmulatedDeviceType::GICR
                | EmulatedDeviceType::VirtioConsole
        )
    }

    /// Converts a usize value to an EmulatedDeviceType.
    pub fn from_usize(value: usize) -> EmulatedDeviceType {
        match value {
            0x0 => EmulatedDeviceType::Dummy,
            0x1 => EmulatedDeviceType::InterruptController,
            0x2 => EmulatedDeviceType::Console,
            0xA => EmulatedDeviceType::IVCChannel,
            0x20 => EmulatedDeviceType::GPPTRedistributor,
            0x21 => EmulatedDeviceType::GPPTDistributor,
            0x22 => EmulatedDeviceType::GPPTITS,
            0xE1 => EmulatedDeviceType::VirtioBlk,
            0xE2 => EmulatedDeviceType::VirtioNet,
            0xE3 => EmulatedDeviceType::VirtioConsole,
            // 0x6 => EmulatedDeviceType::IOMMU,
            // 0x7 => EmulatedDeviceType::ICCSRE,
            // 0x8 => EmulatedDeviceType::SGIR,
            // 0x9 => EmulatedDeviceType::GICR,
            _ => {
                warn!("Unknown emulated device type value: {value}, default to Meta");
                EmulatedDeviceType::Dummy
            }
        }
    }
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
    pub emu_type: EmulatedDeviceType,
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

/// Specifies how the VM should handle interrupts and interrupt controllers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VMInterruptMode {
    /// The VM will not handle interrupts, and the guest OS should not use interrupts.
    #[serde(rename = "no_irq", alias = "no", alias = "none")]
    NoIrq,
    /// The VM will use the emulated interrupt controller to handle interrupts.
    #[serde(rename = "emu", alias = "emulated")]
    Emulated,
    /// The VM will use the passthrough interrupt controller (including GPPT) to handle interrupts.
    #[serde(rename = "passthrough", alias = "pt")]
    Passthrough,
}

impl Default for VMInterruptMode {
    fn default() -> Self {
        Self::NoIrq
    }
}

/// The configuration structure for the guest VM devices.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMDevicesConfig {
    /// Emu device Information
    pub emu_devices: Vec<EmulatedDeviceConfig>,
    /// Passthrough device Information
    pub passthrough_devices: Vec<PassThroughDeviceConfig>,
    /// How the VM should handle interrupts and interrupt controllers.
    #[serde(default)]
    pub interrupt_mode: VMInterruptMode,
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

#[cfg(test)]
mod test;
