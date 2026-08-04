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
//! [`GuestConfig`]: the user-facing guest configuration structure.
//! It is generated from a TOML file and converted to AxVM's internal runtime
//! configuration before VM creation.
#![cfg_attr(not(all(feature = "std", any(windows, unix))), no_std)]

extern crate alloc;
#[macro_use]
extern crate log;

use alloc::{string::String, vec, vec::Vec};

pub use axvm_types::{
    AddressSpacePolicy, EmulatedDeviceConfig, EmulatedDeviceType, PassThroughAddressConfig,
    PassThroughDeviceConfig, PassThroughPortConfig, ReservedAddressConfig, VMBootProtocol,
    VMInterruptMode, VmMemConfig, VmMemMappingType,
};

mod error;

pub use error::*;

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, serde_repr::Serialize_repr, serde_repr::Deserialize_repr)]
#[repr(u8)]
enum VmMemMappingTypeSerde {
    Alloc     = 0,
    Identical = 1,
    Reserved  = 2,
}

impl From<VmMemMappingTypeSerde> for VmMemMappingType {
    fn from(value: VmMemMappingTypeSerde) -> Self {
        match value {
            VmMemMappingTypeSerde::Alloc => Self::MapAlloc,
            VmMemMappingTypeSerde::Identical => Self::MapIdentical,
            VmMemMappingTypeSerde::Reserved => Self::MapReserved,
        }
    }
}

impl From<&VmMemMappingType> for VmMemMappingTypeSerde {
    fn from(value: &VmMemMappingType) -> Self {
        match value {
            VmMemMappingType::MapAlloc => Self::Alloc,
            VmMemMappingType::MapIdentical => Self::Identical,
            VmMemMappingType::MapReserved => Self::Reserved,
        }
    }
}

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VmMemConfigSerde {
    gpa: usize,
    size: usize,
    flags: usize,
    map_type: VmMemMappingTypeSerde,
}

impl From<VmMemConfigSerde> for VmMemConfig {
    fn from(value: VmMemConfigSerde) -> Self {
        Self {
            gpa: value.gpa,
            size: value.size,
            flags: value.flags,
            map_type: value.map_type.into(),
        }
    }
}

impl From<&VmMemConfig> for VmMemConfigSerde {
    fn from(value: &VmMemConfig) -> Self {
        Self {
            gpa: value.gpa,
            size: value.size,
            flags: value.flags,
            map_type: (&value.map_type).into(),
        }
    }
}

mod vm_mem_config_vec_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::*;

    pub fn serialize<S>(value: &[VmMemConfig], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = value.iter().map(VmMemConfigSerde::from).collect::<Vec<_>>();
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<VmMemConfig>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Vec::<VmMemConfigSerde>::deserialize(deserializer)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
}

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum VMBootProtocolSerde {
    #[serde(rename = "direct")]
    #[default]
    Direct,
    #[serde(rename = "multiboot")]
    Multiboot,
    #[serde(rename = "uefi")]
    Uefi,
}

impl From<VMBootProtocolSerde> for VMBootProtocol {
    fn from(value: VMBootProtocolSerde) -> Self {
        match value {
            VMBootProtocolSerde::Direct => Self::Direct,
            VMBootProtocolSerde::Multiboot => Self::Multiboot,
            VMBootProtocolSerde::Uefi => Self::Uefi,
        }
    }
}

impl From<&VMBootProtocol> for VMBootProtocolSerde {
    fn from(value: &VMBootProtocol) -> Self {
        match value {
            VMBootProtocol::Direct => Self::Direct,
            VMBootProtocol::Multiboot => Self::Multiboot,
            VMBootProtocol::Uefi => Self::Uefi,
        }
    }
}

mod vm_boot_protocol_option_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::*;

    pub fn serialize<S>(value: &Option<VMBootProtocol>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value
            .as_ref()
            .map(VMBootProtocolSerde::from)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<VMBootProtocol>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<VMBootProtocolSerde>::deserialize(deserializer)?.map(Into::into))
    }
}

struct BootProtocolSupport {
    protocol: VMBootProtocol,
    supported_arches: &'static [&'static str],
    requires_firmware_path: bool,
    requires_firmware_load_addr: bool,
    optional_firmware_requires_load_addr: bool,
}

const BOOT_PROTOCOL_MATRIX: &[BootProtocolSupport] = &[
    BootProtocolSupport {
        protocol: VMBootProtocol::Direct,
        supported_arches: &["x86_64", "aarch64", "riscv64", "loongarch64"],
        requires_firmware_path: false,
        requires_firmware_load_addr: false,
        optional_firmware_requires_load_addr: false,
    },
    BootProtocolSupport {
        protocol: VMBootProtocol::Multiboot,
        supported_arches: &["x86_64"],
        requires_firmware_path: false,
        requires_firmware_load_addr: false,
        optional_firmware_requires_load_addr: true,
    },
    BootProtocolSupport {
        protocol: VMBootProtocol::Uefi,
        supported_arches: &["x86_64", "loongarch64"],
        requires_firmware_path: true,
        requires_firmware_load_addr: true,
        optional_firmware_requires_load_addr: false,
    },
];

fn boot_protocol_support(protocol: VMBootProtocol) -> &'static BootProtocolSupport {
    BOOT_PROTOCOL_MATRIX
        .iter()
        .find(|support| support.protocol == protocol)
        .expect("all VMBootProtocol variants must be described")
}

fn boot_protocol_name(protocol: VMBootProtocol) -> &'static str {
    match protocol {
        VMBootProtocol::Direct => "direct",
        VMBootProtocol::Multiboot => "multiboot",
        VMBootProtocol::Uefi => "uefi",
    }
}

/// The configuration structure for the guest VM base info.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VMBaseConfig {
    /// VM ID.
    pub id: usize,
    /// VM name.
    pub name: String,
    /// Guest address-space and physical-device assignment model.
    pub guest_type: GuestType,
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

/// The configuration structure for the guest VM kernel.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    /// Guest boot protocol. When omitted, `enable_bios` selects `multiboot`;
    /// otherwise direct boot is used.
    #[serde(default)]
    #[cfg_attr(
        all(feature = "std", any(windows, unix)),
        schemars(with = "Option<VMBootProtocolSerde>")
    )]
    #[serde(with = "vm_boot_protocol_option_serde")]
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
    #[cfg_attr(
        all(feature = "std", any(windows, unix)),
        schemars(with = "Vec<VmMemConfigSerde>")
    )]
    #[serde(with = "vm_mem_config_vec_serde")]
    pub memory_regions: Vec<VmMemConfig>,
    /// Number of memory_regions that came directly from the user-provided config.
    #[serde(skip)]
    pub configured_memory_region_count: usize,
}

impl VMKernelConfig {
    /// Returns the effective boot protocol.
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
    /// generic firmware path carried by `bios_path`.
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
    pub fn validate_boot_config(&self) -> AxVmConfigResult {
        self.validate_boot_config_for_arch(BUILD_TARGET_ARCH)
    }

    fn validate_boot_config_for_arch(&self, arch: &str) -> AxVmConfigResult {
        let protocol = self.effective_boot_protocol();
        if !self.enable_bios {
            if protocol != VMBootProtocol::Direct {
                return Err(AxVmConfigError::BootProtocolConflict {
                    protocol,
                    enable_bios: self.enable_bios,
                });
            }
            return Ok(());
        }

        if protocol == VMBootProtocol::Direct {
            return Err(AxVmConfigError::BootProtocolConflict {
                protocol,
                enable_bios: self.enable_bios,
            });
        }

        let support = boot_protocol_support(protocol);
        if !support.supported_arches.contains(&arch) {
            warn!(
                "boot_protocol={} is only supported on {}; rejecting config on {arch}",
                boot_protocol_name(protocol),
                support.supported_arches.join(", ")
            );
            return Err(AxVmConfigError::UnsupportedBootProtocol {
                protocol,
                arch: arch.into(),
            });
        }

        if support.requires_firmware_path && self.boot_firmware_path().is_none() {
            return Err(AxVmConfigError::MissingFirmwarePath { protocol });
        }

        if support.requires_firmware_load_addr && self.bios_load_addr.is_none() {
            return Err(AxVmConfigError::MissingFirmwareLoadAddress { protocol });
        }

        if support.optional_firmware_requires_load_addr
            && self.bios_path.is_some()
            && self.bios_load_addr.is_none()
        {
            return Err(AxVmConfigError::MissingFirmwareLoadAddress { protocol });
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

/// Determines how the guest receives physical devices.
///
/// Virtualized guests start with no physical-device mappings. Passthrough
/// guests start with all guest-assignable physical devices and then remove the
/// devices listed in [`GuestDevices::disabled`].
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(clap::ValueEnum))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestType {
    /// Only explicitly selected physical devices are mapped into the guest.
    #[default]
    Virtualized,
    /// All guest-assignable physical devices are mapped unless disabled.
    Passthrough,
}

impl GuestType {
    /// Returns AxVM's internal address-space population policy.
    pub const fn address_space_policy(self) -> AddressSpacePolicy {
        match self {
            Self::Virtualized => AddressSpacePolicy::Virtualized,
            Self::Passthrough => AddressSpacePolicy::Passthrough,
        }
    }

    /// Returns AxVM's internal physical-interrupt forwarding policy.
    ///
    /// Passthrough devices still terminate at the guest's virtual interrupt
    /// controller; this mode only enables forwarding their physical sources.
    pub const fn interrupt_mode(self) -> VMInterruptMode {
        match self {
            Self::Virtualized => VMInterruptMode::Emulated,
            Self::Passthrough => VMInterruptMode::Passthrough,
        }
    }
}

/// A structured reference to a host physical device.
///
/// The initial selector is an absolute device-tree path. Raw addresses and IRQ
/// numbers deliberately are not part of the user-facing configuration.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalDeviceRef {
    /// Absolute device-tree path identifying the physical device.
    pub path: String,
}

impl PhysicalDeviceRef {
    fn validate(&self) -> AxVmConfigResult {
        if !self.path.starts_with('/') || self.path == "/" {
            return Err(AxVmConfigError::InvalidPhysicalDevicePath {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Converts this selector into the internal unresolved FDT device form.
    pub fn unresolved_device(&self) -> PassThroughDeviceConfig {
        PassThroughDeviceConfig {
            name: self.path.clone(),
            ..Default::default()
        }
    }
}

/// User-selectable physical-device configuration.
///
/// Virtual platform devices, including the serial port and interrupt
/// controller, are selected by the machine profile and never appear here.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuestDevices {
    /// Physical devices explicitly assigned to the guest.
    pub passthrough: Vec<PhysicalDeviceRef>,
    /// Physical devices removed from a passthrough guest's default assignment.
    pub disabled: Vec<PhysicalDeviceRef>,
}

impl GuestDevices {
    fn validate(&self) -> AxVmConfigResult {
        for device in self.passthrough.iter().chain(&self.disabled) {
            device.validate()?;
        }

        if let Some(device) = self
            .passthrough
            .iter()
            .find(|selected| self.disabled.contains(selected))
        {
            return Err(AxVmConfigError::ConflictingPhysicalDeviceSelection {
                path: device.path.clone(),
            });
        }
        Ok(())
    }

    /// Builds the unresolved FDT selectors consumed by AxVM's boot pipeline.
    pub fn unresolved_passthrough_devices(&self) -> Vec<PassThroughDeviceConfig> {
        self.passthrough
            .iter()
            .map(PhysicalDeviceRef::unresolved_device)
            .collect()
    }

    /// Builds the exclusion paths consumed by AxVM's FDT discovery pipeline.
    pub fn disabled_device_paths(&self) -> Vec<Vec<String>> {
        self.disabled
            .iter()
            .map(|device| vec![device.path.clone()])
            .collect()
    }
}

/// The configuration structure for the guest VM serialized from a toml file provided by user,
/// and then converted to `AxVMConfig` for the VM creation.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuestConfig {
    /// The base configuration for the VM.
    pub base: VMBaseConfig,
    /// The kernel configuration for the VM.
    pub kernel: VMKernelConfig,
    /// The devices configuration for the VM.
    pub devices: GuestDevices,
}

impl GuestConfig {
    /// Deserialize and validate a guest TOML configuration.
    pub fn from_toml(raw_cfg_str: &str) -> AxVmConfigResult<Self> {
        let mut config: Self = toml::from_str(raw_cfg_str)?;
        config.kernel.validate_boot_config()?;
        config.devices.validate()?;
        config.kernel.configured_memory_region_count = config.kernel.memory_regions.len();
        Ok(config)
    }
}

#[cfg(test)]
mod test;
