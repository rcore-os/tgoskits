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

pub use axvm_types::VMBootProtocol;
use axvm_types::{GuestFirmwareKind, InterruptDelivery, VmMachineMode};

mod error;
mod model;

pub use error::*;
pub use model::*;

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum GuestFirmwareKindSerde {
    #[default]
    Auto,
    Fdt,
    Acpi,
}

impl From<GuestFirmwareKindSerde> for GuestFirmwareKind {
    fn from(value: GuestFirmwareKindSerde) -> Self {
        match value {
            GuestFirmwareKindSerde::Auto => Self::Auto,
            GuestFirmwareKindSerde::Fdt => Self::Fdt,
            GuestFirmwareKindSerde::Acpi => Self::Acpi,
        }
    }
}

impl From<GuestFirmwareKind> for GuestFirmwareKindSerde {
    fn from(value: GuestFirmwareKind) -> Self {
        match value {
            GuestFirmwareKind::Auto => Self::Auto,
            GuestFirmwareKind::Fdt => Self::Fdt,
            GuestFirmwareKind::Acpi => Self::Acpi,
        }
    }
}

mod guest_firmware_kind_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::*;

    pub fn serialize<S>(value: &GuestFirmwareKind, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        GuestFirmwareKindSerde::from(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<GuestFirmwareKind, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(GuestFirmwareKindSerde::deserialize(deserializer)?.into())
    }
}

/// Machine-level VM policy parsed from the `[machine]` TOML table.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MachineConfig {
    /// A platform composed entirely from virtual devices.
    Virtual {
        /// Firmware description selected for the guest.
        #[serde(default)]
        #[cfg_attr(
            all(feature = "std", any(windows, unix)),
            schemars(with = "GuestFirmwareKindSerde")
        )]
        #[serde(with = "guest_firmware_kind_serde")]
        firmware: GuestFirmwareKind,
    },
    /// A platform derived from assignable host resources.
    Passthrough {
        /// Firmware description selected for the guest.
        #[serde(default)]
        #[cfg_attr(
            all(feature = "std", any(windows, unix)),
            schemars(with = "GuestFirmwareKindSerde")
        )]
        #[serde(with = "guest_firmware_kind_serde")]
        firmware: GuestFirmwareKind,
        /// Whether assigned physical interrupts bypass the mediated controller.
        #[serde(default)]
        interrupts_passthrough: bool,
    },
}

impl MachineConfig {
    /// Returns the selected machine construction policy.
    pub const fn mode(&self) -> VmMachineMode {
        match self {
            Self::Virtual { .. } => VmMachineMode::Virtual,
            Self::Passthrough { .. } => VmMachineMode::Passthrough,
        }
    }

    /// Returns the selected guest firmware description.
    pub const fn firmware(&self) -> GuestFirmwareKind {
        match self {
            Self::Virtual { firmware } | Self::Passthrough { firmware, .. } => *firmware,
        }
    }

    /// Returns the normalized interrupt-delivery policy.
    pub const fn interrupt_delivery(&self) -> InterruptDelivery {
        match self {
            Self::Virtual { .. } => InterruptDelivery::Mediated,
            Self::Passthrough {
                interrupts_passthrough,
                ..
            } => InterruptDelivery::from_passthrough_flag(*interrupts_passthrough),
        }
    }
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self::Virtual {
            firmware: GuestFirmwareKind::Auto,
        }
    }
}

#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum VMBootProtocolSerde {
    #[serde(rename = "direct", alias = "kernel")]
    #[default]
    Direct,
    #[serde(rename = "multiboot", alias = "bios", alias = "axvm-bios")]
    Multiboot,
    #[serde(rename = "uefi", alias = "efi")]
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
#[serde(deny_unknown_fields)]
pub struct VMBaseConfig {
    /// VM ID.
    pub id: usize,
    /// VM name.
    pub name: String,
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
#[serde(deny_unknown_fields)]
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

/// The configuration structure for the guest VM serialized from a toml file provided by user,
/// and then converted to `AxVMConfig` for the VM creation.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxVMCrateConfig {
    /// Machine construction, firmware, and external interrupt policy.
    pub machine: MachineConfig,
    /// The base configuration for the VM.
    pub base: VMBaseConfig,
    /// The kernel configuration for the VM.
    pub kernel: VMKernelConfig,
    /// Explicit guest memory layout.
    pub memory: MemoryConfig,
    /// The devices configuration for the VM.
    pub devices: VMDevicesConfig,
}

impl AxVMCrateConfig {
    /// Deserializes and validates a TOML configuration for the current build target.
    ///
    /// # Errors
    ///
    /// Returns [`AxVmConfigError`] when the TOML shape or any target-specific policy is invalid.
    pub fn from_toml(raw_cfg_str: &str) -> AxVmConfigResult<Self> {
        Self::from_toml_for_target_arch(raw_cfg_str, BUILD_TARGET_ARCH)
    }

    /// Deserializes and validates a TOML configuration for an explicit target architecture.
    ///
    /// Host-side build tools use this entry point because Cargo compiles their dependencies for
    /// the host, while the embedded VM configuration must obey the eventual Axvisor target.
    ///
    /// # Errors
    ///
    /// Returns [`AxVmConfigError`] when the TOML shape or any policy for `target_arch` is invalid.
    pub fn from_toml_for_target_arch(
        raw_cfg_str: &str,
        target_arch: &str,
    ) -> AxVmConfigResult<Self> {
        let config: AxVMCrateConfig = toml::from_str(raw_cfg_str)?;
        config.kernel.validate_boot_config_for_arch(target_arch)?;
        config.validate_for_arch(target_arch)?;
        Ok(config)
    }

    fn validate_for_arch(&self, target_arch: &str) -> AxVmConfigResult {
        for device in &self.devices.disable_defaults {
            if device != "console" {
                return Err(AxVmConfigError::UnsupportedDefaultDevice {
                    device: device.clone(),
                });
            }
        }
        validate_memory_regions(&self.memory.regions, self.machine.mode(), target_arch)
    }
}

#[derive(Clone, Copy)]
struct ValidatedGuestMemoryRange {
    guest_base: u64,
    size: u64,
    end: u64,
}

fn validate_memory_regions(
    regions: &[MemoryRegionConfig],
    mode: VmMachineMode,
    arch: &str,
) -> AxVmConfigResult {
    let ranges = regions
        .iter()
        .map(|region| validate_memory_region(region, mode, arch))
        .collect::<AxVmConfigResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    for (index, first) in ranges.iter().enumerate() {
        for second in &ranges[index + 1..] {
            if first.overlaps(*second) {
                return Err(AxVmConfigError::OverlappingMemoryRegions {
                    first_guest_base: first.guest_base,
                    first_size: first.size,
                    second_guest_base: second.guest_base,
                    second_size: second.size,
                });
            }
        }
    }
    Ok(())
}

fn validate_memory_region(
    region: &MemoryRegionConfig,
    mode: VmMachineMode,
    arch: &str,
) -> AxVmConfigResult<Option<ValidatedGuestMemoryRange>> {
    let Some(end) = region.guest_base.checked_add(region.size) else {
        return Err(AxVmConfigError::InvalidMemoryRegion {
            guest_base: region.guest_base,
            size: region.size,
        });
    };
    if region.size == 0 {
        return Err(AxVmConfigError::InvalidMemoryRegion {
            guest_base: region.guest_base,
            size: region.size,
        });
    }
    if matches!(region.backing, MemoryBackingConfig::IdentityAllocate) {
        if region.guest_base != 0 {
            return Err(AxVmConfigError::InvalidIdentityAllocatedMemoryBase {
                guest_base: region.guest_base,
            });
        }
        if arch != "x86_64" || mode != VmMachineMode::Passthrough {
            return Err(AxVmConfigError::UnsupportedIdentityAllocatedMemory {
                arch: String::from(arch),
                mode,
            });
        }
        return Ok(None);
    }
    if let MemoryBackingConfig::Host { host_base } | MemoryBackingConfig::Shared { host_base } =
        region.backing
        && host_base.checked_add(region.size).is_none()
    {
        return Err(AxVmConfigError::InvalidMemoryBacking {
            host_base,
            size: region.size,
        });
    }
    Ok(Some(ValidatedGuestMemoryRange {
        guest_base: region.guest_base,
        size: region.size,
        end,
    }))
}

impl ValidatedGuestMemoryRange {
    fn overlaps(self, other: Self) -> bool {
        self.guest_base < other.end && other.guest_base < self.end
    }
}

#[cfg(test)]
mod test;
