//! Typed memory and device requests parsed from VM TOML.

use alloc::{string::String, vec::Vec};

/// Access permissions assigned to one guest memory region.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryPermissions(u8);

impl MemoryPermissions {
    const READ: u8 = 1 << 0;
    const WRITE: u8 = 1 << 1;
    const EXECUTE: u8 = 1 << 2;

    /// Returns whether guest reads are permitted.
    pub const fn readable(self) -> bool {
        self.0 & Self::READ != 0
    }

    /// Returns whether guest writes are permitted.
    pub const fn writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    /// Returns whether guest instruction fetches are permitted.
    pub const fn executable(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }

    /// Returns the normalized `rwx` representation.
    pub fn as_str(self) -> &'static str {
        match self.0 {
            0b001 => "r",
            0b011 => "rw",
            0b101 => "rx",
            0b111 => "rwx",
            _ => "",
        }
    }
}

impl Default for MemoryPermissions {
    fn default() -> Self {
        Self(Self::READ | Self::WRITE)
    }
}

impl TryFrom<&str> for MemoryPermissions {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut bits = 0;
        let mut previous = None;
        for permission in value.bytes() {
            if previous.is_some_and(|previous| previous >= permission) {
                return Err("memory permissions must be ordered and contain no duplicates");
            }
            previous = Some(permission);
            bits |= match permission {
                b'r' => Self::READ,
                b'w' => Self::WRITE,
                b'x' => Self::EXECUTE,
                _ => return Err("memory permissions may contain only 'r', 'w', and 'x'"),
            };
        }
        if bits & Self::READ == 0 {
            return Err("memory permissions must include read access");
        }
        Ok(Self(bits))
    }
}

impl serde::Serialize for MemoryPermissions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for MemoryPermissions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <&str>::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// Physical backing selected for one guest memory region.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MemoryBackingConfig {
    /// Allocate zeroed VM-owned memory.
    #[default]
    Allocate,
    /// Allocate zeroed VM-owned memory and expose it at an identical guest address.
    ///
    /// This placement supports DMA from assigned x86 devices without an IOMMU.
    /// `guest_base` must be zero because the allocator determines the final address.
    IdentityAllocate,
    /// Map an explicitly assigned host physical range.
    Host {
        /// First host physical address backing the region.
        host_base: u64,
    },
    /// Map explicitly shared host memory without taking device ownership.
    Shared {
        /// First host physical address backing the region.
        host_base: u64,
    },
    /// Reserve an identity-backed range already owned by platform policy.
    Reserved,
}

/// One explicit guest RAM, reserved, or shared-memory region.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRegionConfig {
    /// First guest physical address.
    pub guest_base: u64,
    /// Region length in bytes.
    pub size: u64,
    /// Guest access permissions.
    #[serde(default)]
    #[cfg_attr(all(feature = "std", any(windows, unix)), schemars(with = "String"))]
    pub permissions: MemoryPermissions,
    /// Physical backing policy.
    #[serde(default)]
    pub backing: MemoryBackingConfig,
}

/// Explicit guest memory layout.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Guest memory regions in boot-priority order.
    #[serde(default)]
    pub regions: Vec<MemoryRegionConfig>,
}

/// A stable selector used to deny or select one host firmware device.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DeviceSelectorConfig {
    /// Select an FDT node and its descendants by absolute path.
    FdtPath {
        /// Absolute node path.
        value: String,
    },
    /// Select an ACPI namespace object and its descendants.
    AcpiPath {
        /// Fully qualified ACPI namespace path.
        value: String,
    },
    /// Select nodes matching a firmware compatible identifier.
    Compatible {
        /// Compatible or hardware identifier.
        value: String,
    },
    /// Select the owner of an overlapping host MMIO range.
    Mmio {
        /// First host physical address.
        base: u64,
        /// Range length in bytes.
        size: u64,
    },
    /// Select the owner of one host interrupt identifier.
    Interrupt {
        /// Platform hardware interrupt identifier.
        intid: u32,
    },
}

impl DeviceSelectorConfig {
    /// Returns the selector text for path and compatible selectors.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::FdtPath { value } | Self::AcpiPath { value } | Self::Compatible { value } => {
                Some(value)
            }
            Self::Mmio { .. } | Self::Interrupt { .. } => None,
        }
    }
}

/// Resource source selected for one virtual device instance.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum VirtualDeviceSourceConfig {
    /// Match a host template in passthrough mode and allocate on no match.
    #[default]
    Auto,
    /// Always allocate new guest resources.
    Allocate,
    /// Reuse one explicit FDT node as the guest template.
    FdtPath {
        /// Absolute source node path.
        value: String,
    },
    /// Reuse one explicit ACPI object as the guest template.
    AcpiPath {
        /// Fully qualified source object path.
        value: String,
    },
    /// Reuse the first unconsumed matching compatible device.
    Compatible {
        /// Compatible or hardware identifier.
        value: String,
    },
}

/// Host-console receive ownership policy.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleRxMode {
    /// This VM is the sole consumer of host console input.
    #[default]
    Exclusive,
    /// The virtual console does not receive host input.
    Disabled,
}

/// Host-console transmit ownership policy.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleTxMode {
    /// Serialize output with other host-console writers.
    #[default]
    Shared,
    /// This VM is the sole host-console writer.
    Exclusive,
    /// Discard guest output.
    Disabled,
}

/// Backend capability selected for one virtual device.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum VirtualDeviceBackendConfig {
    /// The model needs no external backend.
    #[default]
    None,
    /// Connect a serial model to the hypervisor's host console service.
    HostConsole {
        /// Host input ownership.
        #[serde(default)]
        rx: ConsoleRxMode,
        /// Host output ownership.
        #[serde(default)]
        tx: ConsoleTxMode,
    },
}

/// One virtual device instance requested by stable identity and model name.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDeviceConfig {
    /// Stable per-VM instance identity used for deterministic allocation.
    pub id: String,
    /// Registered virtual device model name.
    pub model: String,
    /// Host-template or dynamic-allocation policy.
    #[serde(default)]
    pub source: VirtualDeviceSourceConfig,
    /// External backend capability.
    #[serde(default)]
    pub backend: VirtualDeviceBackendConfig,
}

/// Virtual-device and host-device filtering policy.
#[cfg_attr(all(feature = "std", any(windows, unix)), derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VMDevicesConfig {
    /// Architecture-profile defaults disabled by model name.
    #[serde(default)]
    pub disable_defaults: Vec<String>,
    /// Host devices excluded before virtual replacement and passthrough.
    #[serde(default)]
    pub deny: Vec<DeviceSelectorConfig>,
    /// Explicit virtual devices in addition to enabled profile defaults.
    #[serde(default, rename = "virtual")]
    pub virtual_devices: Vec<VirtualDeviceConfig>,
}
