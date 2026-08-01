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
    /// A dedicated VM vCPU has no explicit CPU-affinity mask.
    #[error("dedicated VM {vm_id} vCPU {vcpu_id} requires an explicit CPU-affinity mask")]
    MissingDedicatedCpuAffinity {
        /// VM whose dedicated placement is incomplete.
        vm_id: usize,
        /// vCPU whose affinity is missing.
        vcpu_id: usize,
    },
    /// A vCPU has an empty CPU-affinity mask.
    #[error("VM {vm_id} vCPU {vcpu_id} has an empty CPU-affinity mask")]
    EmptyCpuAffinity {
        /// VM containing the empty affinity.
        vm_id: usize,
        /// vCPU containing the empty affinity.
        vcpu_id: usize,
    },
    /// A vCPU affinity names no CPU available to the hypervisor.
    #[error(
        "VM {vm_id} vCPU {vcpu_id} CPU mask {requested:#x} is outside available mask \
         {available:#x}"
    )]
    CpuAffinityUnavailable {
        /// VM containing the unavailable affinity.
        vm_id: usize,
        /// vCPU containing the unavailable affinity.
        vcpu_id: usize,
        /// Requested physical-CPU mask.
        requested: usize,
        /// Physical CPUs available to the hypervisor.
        available: usize,
    },
    /// The configured vCPU count and affinity count differ.
    #[error(
        "VM {vm_id} configures {cpu_num} vCPUs but provides {affinity_count} CPU-affinity masks"
    )]
    CpuAffinityCountMismatch {
        /// VM containing the inconsistent configuration.
        vm_id: usize,
        /// Configured vCPU count.
        cpu_num: usize,
        /// Number of configured affinity masks.
        affinity_count: usize,
    },
    /// Two dedicated VMs reserve at least one common physical CPU.
    #[error(
        "dedicated VM {vm_id} overlaps dedicated VM {conflicting_vm_id} on CPU mask {overlap:#x}"
    )]
    DedicatedCpuConflict {
        /// VM being validated.
        vm_id: usize,
        /// Previously validated VM with an overlapping reservation.
        conflicting_vm_id: usize,
        /// Physical CPUs reserved by both VMs.
        overlap: usize,
    },
    /// Removing dedicated CPUs leaves a shared vCPU with no runnable CPU.
    #[error(
        "VM {vm_id} vCPU {vcpu_id} CPU mask {requested:#x} is exhausted by dedicated mask \
         {reserved:#x}"
    )]
    SharedCpuAffinityExhausted {
        /// Shared VM whose vCPU cannot be placed.
        vm_id: usize,
        /// vCPU whose effective mask would be empty.
        vcpu_id: usize,
        /// Requested or available mask before removing reservations.
        requested: usize,
        /// Physical CPUs reserved by dedicated VMs.
        reserved: usize,
    },
}

impl From<toml::de::Error> for AxVmConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlParse {
            detail: error.to_string(),
        }
    }
}
