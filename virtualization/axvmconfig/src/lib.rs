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

//! [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/arceos-umhv)
//! [VM](https://github.com/arceos-hypervisor/axvm) config module.
//! [`AxVMCrateConfig`]: the configuration structure for the VM.
//! It is generated from toml file, and then converted to `AxVMConfig` for the VM creation.
#![cfg_attr(not(all(feature = "std", any(windows, unix))), no_std)]

extern crate alloc;
#[macro_use]
extern crate log;

use alloc::{string::String, vec::Vec};

use ax_errno::AxResult;
pub use axvm_types::EmulatedDeviceType;

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
/// A part of `AxVMConfig`, which represents guest VM type.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum VMType {
    /// Host VM, used for boot from Linux like Jailhouse do, named "type1.5".
    VMTHostVM = 0,
    /// Guest RTOS, generally a simple guest OS with most of the resource passthrough.
    #[default]
    VMTRTOS   = 1,
    /// Guest Linux, generally a full-featured guest OS with complicated device emulation requirements.
    VMTLinux  = 2,
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

/// The type of memory mapping used for VM memory regions.
///
/// Defines how virtual machine memory regions are mapped to host physical memory.
/// This affects memory allocation and management strategies in the hypervisor.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, serde_repr::Serialize_repr, serde_repr::Deserialize_repr)]
#[repr(u8)]
pub enum VmMemMappingType {
    /// The memory region is allocated by the VM monitor.
    MapAlloc     = 0,
    /// The memory region is identical to the host physical memory region.
    MapIdentical = 1,
    /// The memory region is reserved memory for the guest OS.
    MapReserved  = 2,
}

/// The default value of `VmMemMappingType` is `MapAlloc`.
impl Default for VmMemMappingType {
    fn default() -> Self {
        Self::MapAlloc
    }
}

/// Configuration for a virtual machine memory region.
///
/// Represents a contiguous memory region within the guest's physical address space.
/// Each region has specific properties including address, size, access permissions,
/// and mapping type that determine how it's handled by the hypervisor.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmMemConfig {
    /// The start address of the memory region in GPA (Guest Physical Address).
    pub gpa: usize,
    /// The size of the memory region in bytes.
    pub size: usize,
    /// The mappings flags of the memory region, refers to `MappingFlags` provided by `axaddrspace`.
    /// Defines access permissions (read, write, execute) and caching behavior.
    pub flags: usize,
    /// The type of memory mapping.
    /// Determines whether memory is allocated dynamically or mapped identically.
    pub map_type: VmMemMappingType,
}

/// A part of `AxVMConfig`, which represents the configuration of an emulated device for a virtual machine.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
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
    #[cfg_attr(all(feature = "std", any(windows, unix)), schemars(with = "u8"))]
    #[serde(with = "emu_device_type_serde")]
    pub emu_type: EmulatedDeviceType,
    /// The config_list of the device
    pub cfg_list: Vec<usize>,
}

mod emu_device_type_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::*;

    pub fn serialize<S>(emu_type: &EmulatedDeviceType, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(*emu_type as u8)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<EmulatedDeviceType, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = usize::from(u8::deserialize(deserializer)?);
        match EmulatedDeviceType::from_usize(value) {
            Some(emu_type) => Ok(emu_type),
            None => {
                warn!("Unknown emulated device type value: {value}, default to Meta");
                Ok(EmulatedDeviceType::Dummy)
            }
        }
    }
}

/// A part of `AxVMConfig`, which represents the configuration of a pass-through device for a virtual machine.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PassThroughDeviceConfig {
    /// The name of the device.
    pub name: String,
    /// The base GPA (Guest Physical Address) of the device.
    #[serde(default)]
    pub base_gpa: usize,
    /// The base HPA (Host Physical Address) of the device.
    #[serde(default)]
    pub base_hpa: usize,
    /// The address length of the device.
    #[serde(default)]
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    #[serde(default)]
    pub irq_id: usize,
}

/// A part of `AxVMConfig`, which represents the configuration of a pass-through address for a virtual machine.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PassThroughAddressConfig {
    /// The base GPA (Guest Physical Address).
    #[serde(default)]
    pub base_gpa: usize,
    /// The address length.
    #[serde(default)]
    pub length: usize,
}

/// The configuration structure for the guest VM base info.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
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
    ///   For example, [0x0101, 0x0010] means:
    ///   - vCpu0 can be scheduled at pCpu0 and pCpu2;
    ///   - vCpu1 will only be scheduled at pCpu1;
    ///
    ///   It will phrase an error if the number of vCpus is not equal to the length of `phys_cpu_sets` array.
    pub phys_cpu_sets: Option<Vec<usize>>,
}

/// Describes how a guest VM should enter its boot image.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VMBootProtocol {
    /// Enter the configured kernel entry directly without a firmware image.
    #[serde(rename = "direct", alias = "kernel")]
    #[default]
    Direct,
    /// Use the legacy x86 axvm-bios/multiboot trampoline.
    #[serde(rename = "multiboot", alias = "bios", alias = "axvm-bios")]
    Multiboot,
    /// Load an external UEFI firmware image and enter it without multiboot patching.
    #[serde(rename = "uefi", alias = "efi")]
    Uefi,
}

/// The configuration structure for the guest VM kernel.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMKernelConfig {
    /// The entry point of the kernel image.
    pub entry_point: usize,
    /// The file path of the kernel image.
    pub kernel_path: String,
    /// The load address of the kernel image.
    pub kernel_load_addr: usize,
    /// Whether to enable BIOS boot flow for this VM.
    #[serde(default)]
    pub enable_bios: bool,
    /// Guest boot protocol. When omitted, legacy configs use `multiboot` if
    /// `enable_bios = true`, otherwise `direct`.
    #[serde(default)]
    pub boot_protocol: Option<VMBootProtocol>,
    /// The file path of the BIOS image, `None` if not used.
    #[serde(default)]
    pub bios_path: Option<String>,
    /// The file path of the UEFI firmware image, `None` if not used.
    #[serde(default)]
    pub uefi_firmware_path: Option<String>,
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
    /// Number of memory_regions that came directly from the user-provided config.
    #[serde(skip)]
    pub configured_memory_region_count: usize,
}

impl VMKernelConfig {
    /// Returns the effective boot protocol after applying compatibility defaults.
    pub fn effective_boot_protocol(&self) -> VMBootProtocol {
        self.boot_protocol.unwrap_or({
            if self.enable_bios {
                VMBootProtocol::Multiboot
            } else {
                VMBootProtocol::Direct
            }
        })
    }

    /// Returns the configured boot firmware image path.
    ///
    /// For UEFI, prefer the explicit UEFI firmware path and fall back to the
    /// legacy BIOS path for compatibility with older configs.
    pub fn boot_firmware_path(&self) -> Option<&str> {
        match self.effective_boot_protocol() {
            VMBootProtocol::Uefi => self
                .uefi_firmware_path
                .as_deref()
                .or(self.bios_path.as_deref()),
            _ => self.bios_path.as_deref(),
        }
    }

    /// Validate that the configured boot protocol has the firmware inputs it needs.
    pub fn validate_boot_config(&self) -> AxResult<()> {
        self.validate_boot_config_for_arch(BUILD_TARGET_ARCH)
    }

    fn validate_boot_config_for_arch(&self, arch: &str) -> AxResult<()> {
        if !self.enable_bios {
            if matches!(
                self.effective_boot_protocol(),
                VMBootProtocol::Multiboot | VMBootProtocol::Uefi
            ) {
                return Err(ax_errno::ax_err_type!(
                    InvalidInput,
                    "boot_protocol requires enable_bios = true"
                ));
            }
            return Ok(());
        }

        match self.effective_boot_protocol() {
            VMBootProtocol::Uefi => {
                if arch != "x86_64" {
                    warn!(
                        "boot_protocol=uefi is only supported on x86_64; rejecting config on \
                         {arch}"
                    );
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "UEFI boot is only supported on x86_64"
                    ));
                }
                if self.boot_firmware_path().is_none() {
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "UEFI boot requires uefi_firmware_path or legacy bios_path"
                    ));
                }
                if self.bios_load_addr.is_none() {
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "UEFI boot requires bios_load_addr"
                    ));
                }
            }
            VMBootProtocol::Multiboot => {
                if arch != "x86_64" {
                    warn!(
                        "boot_protocol=multiboot is only supported on x86_64; rejecting config on \
                         {arch}"
                    );
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "multiboot firmware boot is only supported on x86_64"
                    ));
                }
                if self.bios_path.is_some() && self.bios_load_addr.is_none() {
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "external BIOS boot requires bios_load_addr"
                    ));
                }
            }
            VMBootProtocol::Direct => {
                if self.enable_bios {
                    return Err(ax_errno::ax_err_type!(
                        InvalidInput,
                        "direct boot must not set enable_bios = true"
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
const BUILD_TARGET_ARCH: &str = "x86_64";

#[cfg(target_arch = "aarch64")]
const BUILD_TARGET_ARCH: &str = "aarch64";

#[cfg(target_arch = "riscv64")]
const BUILD_TARGET_ARCH: &str = "riscv64";

#[cfg(target_arch = "loongarch64")]
const BUILD_TARGET_ARCH: &str = "loongarch64";

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
const BUILD_TARGET_ARCH: &str = "unknown";

/// Specifies how the VM should handle interrupts and interrupt controllers.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VMInterruptMode {
    /// The VM will not handle interrupts, and the guest OS should not use interrupts.
    #[serde(rename = "no_irq", alias = "no", alias = "none")]
    #[default]
    NoIrq,
    /// The VM will use the emulated interrupt controller to handle interrupts.
    #[serde(rename = "emu", alias = "emulated")]
    Emulated,
    /// The VM will use the passthrough interrupt controller (including GPPT) to handle interrupts.
    #[serde(rename = "passthrough", alias = "pt")]
    Passthrough,
}

/// The configuration structure for the guest VM devices.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VMDevicesConfig {
    /// Emu device Information
    pub emu_devices: Vec<EmulatedDeviceConfig>,
    /// Passthrough device Information
    pub passthrough_devices: Vec<PassThroughDeviceConfig>,
    /// How the VM should handle interrupts and interrupt controllers.
    #[serde(default)]
    pub interrupt_mode: VMInterruptMode,
    /// we would not like to pass through devices
    #[serde(default)]
    pub excluded_devices: Vec<Vec<String>>,
    /// we would like to pass through address
    #[serde(default)]
    pub passthrough_addresses: Vec<PassThroughAddressConfig>,
}

/// The configuration structure for the guest VM serialized from a toml file provided by user,
/// and then converted to `AxVMConfig` for the VM creation.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
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
        let mut config: AxVMCrateConfig = toml::from_str(raw_cfg_str).map_err(|err| {
            warn!("Config TOML parse error {:?}", err.message());
            ax_errno::ax_err_type!(InvalidInput, alloc::format!("Error details {err:?}"))
        })?;
        config.kernel.validate_boot_config()?;
        config.kernel.configured_memory_region_count = config.kernel.memory_regions.len();
        Ok(config)
    }
}

#[cfg(test)]
mod test;
