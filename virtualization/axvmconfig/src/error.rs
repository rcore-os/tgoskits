//! AxVM configuration error contract.

use alloc::string::{String, ToString};

use crate::VMBootProtocol;

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
    /// A physical-device selector is not an absolute, concrete device-tree path.
    #[error("physical device path must be absolute and must not select the root: {path}")]
    InvalidPhysicalDevicePath {
        /// The invalid path.
        path: String,
    },
    /// The same physical device was both assigned and disabled.
    #[error("physical device is present in both passthrough and disabled lists: {path}")]
    ConflictingPhysicalDeviceSelection {
        /// The conflicting path.
        path: String,
    },
    /// A virtual-device ID is empty or contains unsupported characters.
    #[error("invalid virtual device id: {id}")]
    InvalidVirtualDeviceId { id: String },
    /// A virtual-device model is not a canonical lower-case model name.
    #[error("invalid virtual device model name: {model}")]
    InvalidVirtualDeviceModel { model: String },
    /// More than one configured virtual device uses the same stable ID.
    #[error("duplicate virtual device id: {id}")]
    DuplicateVirtualDeviceId { id: String },
    /// A model option attempts to specify a framework-owned numeric resource.
    #[error("virtual device {id} cannot configure framework-owned resource option {option}")]
    ForbiddenVirtualDeviceResourceOption { id: String, option: String },
}

impl From<toml::de::Error> for AxVmConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlParse {
            detail: error.to_string(),
        }
    }
}
