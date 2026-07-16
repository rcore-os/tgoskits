//! AxVM configuration error contract.

use alloc::string::{String, ToString};

use crate::{VMBootProtocol, VmMachineMode};

/// Result type returned by AxVM configuration operations.
pub type AxVmConfigResult<T = ()> = Result<T, AxVmConfigError>;

/// Errors reported while parsing or validating an AxVM configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AxVmConfigError {
    /// The input is not valid TOML or does not match the configuration schema.
    #[error("failed to parse VM TOML configuration: {detail}")]
    TomlParse {
        /// Parser diagnostic including the failing key or source location when available.
        detail: String,
    },
    /// The selected protocol conflicts with the legacy BIOS enable flag.
    #[error("boot protocol {protocol:?} conflicts with enable_bios = {enable_bios}")]
    BootProtocolConflict {
        /// The selected boot protocol.
        protocol: VMBootProtocol,
        /// Whether the legacy BIOS flow was enabled.
        enable_bios: bool,
    },
    /// The selected boot protocol is not available on the target architecture.
    #[error("boot protocol {protocol:?} is not supported on architecture {arch}")]
    UnsupportedBootProtocol {
        /// The unsupported boot protocol.
        protocol: VMBootProtocol,
        /// The target architecture name.
        arch: String,
    },
    /// Firmware boot was selected without a firmware image path.
    #[error("boot protocol {protocol:?} requires uefi_firmware_path or the compatible bios_path")]
    MissingFirmwarePath {
        /// The boot protocol requiring a firmware image.
        protocol: VMBootProtocol,
    },
    /// Firmware boot was selected without a load address.
    #[error("boot protocol {protocol:?} requires a firmware load address in bios_load_addr")]
    MissingFirmwareLoadAddress {
        /// The boot protocol requiring a firmware load address.
        protocol: VMBootProtocol,
    },
    /// One configured guest memory range was empty or overflowed.
    #[error("invalid guest memory range at {guest_base:#x} with size {size:#x}")]
    InvalidMemoryRegion {
        /// First guest physical address.
        guest_base: u64,
        /// Region length.
        size: u64,
    },
    /// One explicit host backing range overflowed.
    #[error("invalid host memory backing at {host_base:#x} with size {size:#x}")]
    InvalidMemoryBacking {
        /// First host physical address.
        host_base: u64,
        /// Region length.
        size: u64,
    },
    /// Identity-allocated memory used a fixed guest address instead of the zero placeholder.
    #[error("identity-allocated memory requires guest_base = 0, got {guest_base:#x}")]
    InvalidIdentityAllocatedMemoryBase {
        /// Unsupported configured guest base.
        guest_base: u64,
    },
    /// Identity-allocated memory was requested by an unsupported machine.
    #[error(
        "identity-allocated memory requires an x86_64 passthrough machine, got {arch} {mode:?}"
    )]
    UnsupportedIdentityAllocatedMemory {
        /// Build target architecture.
        arch: String,
        /// Configured machine policy.
        mode: VmMachineMode,
    },
    /// Two configured regions claim at least one common guest physical address.
    #[error(
        "guest memory ranges at {first_guest_base:#x}/{first_size:#x} and \
         {second_guest_base:#x}/{second_size:#x} overlap"
    )]
    OverlappingMemoryRegions {
        /// First conflicting guest physical address.
        first_guest_base: u64,
        /// Length of the first conflicting region.
        first_size: u64,
        /// Second conflicting guest physical address.
        second_guest_base: u64,
        /// Length of the second conflicting region.
        second_size: u64,
    },
    /// A mandatory architecture-profile device was listed as disableable.
    #[error("default device '{device}' cannot be disabled; only 'console' is optional")]
    UnsupportedDefaultDevice {
        /// Unsupported profile device name supplied by the configuration.
        device: String,
    },
}

impl From<toml::de::Error> for AxVmConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlParse {
            detail: error.to_string(),
        }
    }
}
