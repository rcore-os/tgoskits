//! AxVM-owned error contract.

use alloc::{format, string::String};
use core::fmt::Display;

use axaddrspace::AddrSpaceError;
use axdevice::DeviceManagerError;
use axdevice_base::{DeviceError, IrqError, RegistryError};

use crate::{VMId, VmStatus};

/// Result type returned by AxVM operations.
pub type AxVmResult<T = ()> = Result<T, AxVmError>;

/// Errors reported by AxVM to a hypervisor application.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AxVmError {
    /// The VM configuration is internally inconsistent or malformed.
    #[error("invalid VM configuration: {detail}")]
    InvalidConfig { detail: String },
    /// An operation received an invalid argument.
    #[error("invalid input for {operation}: {detail}")]
    InvalidInput {
        operation: &'static str,
        detail: String,
    },
    /// Runtime state does not allow the requested operation.
    #[error("invalid state for {operation}: {detail}")]
    InvalidState {
        operation: &'static str,
        detail: String,
    },
    /// A lifecycle transition is not valid.
    #[error("invalid VM lifecycle transition during {operation}: {from:?} -> {to:?}")]
    InvalidTransition {
        from: VmStatus,
        to: VmStatus,
        operation: &'static str,
    },
    /// No registered VM has the requested identifier.
    #[error("VM {vm_id} was not found")]
    VmNotFound { vm_id: VMId },
    /// A required VM resource is unavailable.
    #[error("VM resource {resource} is unavailable: {detail}")]
    ResourceUnavailable {
        resource: &'static str,
        detail: String,
    },
    /// A VM resource conflicts with an existing resource.
    #[error("VM resource {resource} conflicts: {detail}")]
    ResourceConflict {
        resource: &'static str,
        detail: String,
    },
    /// The requested operation is not implemented by this host or backend.
    #[error("unsupported VM operation {operation}: {detail}")]
    Unsupported {
        operation: &'static str,
        detail: String,
    },
    /// Host memory allocation failed.
    #[error("out of memory while {operation}")]
    OutOfMemory { operation: &'static str },
    /// Guest boot preparation or image loading failed.
    #[error("guest boot operation {operation} failed: {detail}")]
    Boot {
        operation: &'static str,
        detail: String,
    },
    /// Guest memory or nested-paging work failed.
    #[error("VM memory operation {operation} failed: {detail}")]
    Memory {
        operation: &'static str,
        detail: String,
    },
    /// Virtual-device setup or emulation failed.
    #[error("VM device operation {operation} failed: {detail}")]
    Device {
        operation: &'static str,
        detail: String,
    },
    /// A virtual CPU operation failed.
    #[error("vCPU operation {operation} failed: {detail}")]
    Vcpu {
        operation: &'static str,
        detail: String,
    },
    /// Interrupt routing or injection failed.
    #[error("VM interrupt operation {operation} failed: {detail}")]
    Interrupt {
        operation: &'static str,
        detail: String,
    },
    /// No vCPU is currently published on this host CPU.
    #[error("no vCPU is published on the current host CPU")]
    CurrentVcpuUnavailable,
    /// The hard-IRQ publication bitmap cannot represent this interrupt vector.
    #[error("current-vCPU interrupt vector {vector:#x} exceeds the fixed pending bitmap")]
    CurrentVcpuInterruptOutOfRange {
        /// Rejected interrupt vector.
        vector: usize,
    },
    /// A host capability used by AxVM failed.
    #[error("host operation {operation} failed: {detail}")]
    Host {
        operation: &'static str,
        detail: String,
    },
}

impl AxVmError {
    pub(crate) const fn invalid_transition(
        from: VmStatus,
        to: VmStatus,
        operation: &'static str,
    ) -> Self {
        Self::InvalidTransition {
            from,
            to,
            operation,
        }
    }

    pub(crate) fn invalid_config(detail: impl Display) -> Self {
        Self::InvalidConfig {
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn invalid_input(operation: &'static str, detail: impl Display) -> Self {
        Self::InvalidInput {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn invalid_state(operation: &'static str, detail: impl Display) -> Self {
        Self::InvalidState {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn resource_unavailable(resource: &'static str, detail: impl Display) -> Self {
        Self::ResourceUnavailable {
            resource,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn resource_conflict(resource: &'static str, detail: impl Display) -> Self {
        Self::ResourceConflict {
            resource,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn unsupported(operation: &'static str, detail: impl Display) -> Self {
        Self::Unsupported {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn memory(operation: &'static str, detail: impl Display) -> Self {
        Self::Memory {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn device(operation: &'static str, detail: impl Display) -> Self {
        Self::Device {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn vcpu(operation: &'static str, detail: impl Display) -> Self {
        Self::Vcpu {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn interrupt(operation: &'static str, detail: impl Display) -> Self {
        Self::Interrupt {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn host(operation: &'static str, detail: impl Display) -> Self {
        Self::Host {
            operation,
            detail: format!("{detail}"),
        }
    }

    pub(crate) fn from_addrspace(operation: &'static str, error: AddrSpaceError) -> Self {
        match error {
            AddrSpaceError::OutOfRange { .. }
            | AddrSpaceError::Unaligned { .. }
            | AddrSpaceError::AddressOverflow { .. }
            | AddrSpaceError::InvalidMapping => Self::invalid_input(operation, error),
            AddrSpaceError::MappingConflict => Self::resource_conflict(
                "guest address range",
                format_args!("{operation} failed: {error}"),
            ),
            AddrSpaceError::MappingState
            | AddrSpaceError::Unmapped { .. }
            | AddrSpaceError::InsufficientAccess { .. } => Self::memory(operation, error),
        }
    }
}

impl From<DeviceError> for AxVmError {
    fn from(error: DeviceError) -> Self {
        match error {
            DeviceError::InvalidInput { operation, detail } => {
                Self::InvalidInput { operation, detail }
            }
            DeviceError::InvalidData { detail, .. } => Self::InvalidConfig { detail },
            DeviceError::Unsupported { operation, detail } => {
                Self::Unsupported { operation, detail }
            }
            DeviceError::OutOfMemory { operation } => Self::OutOfMemory { operation },
            DeviceError::ResourceBusy {
                operation,
                resource,
            } => {
                Self::resource_conflict("device resource", format_args!("{operation}: {resource}"))
            }
            error => Self::device("access virtual device", error),
        }
    }
}

impl From<IrqError> for AxVmError {
    fn from(error: IrqError) -> Self {
        Self::interrupt("route virtual device interrupt", error)
    }
}

impl From<RegistryError> for AxVmError {
    fn from(error: RegistryError) -> Self {
        match error {
            RegistryError::AddressConflict { .. } => {
                Self::resource_conflict("device address range", error)
            }
            RegistryError::InvalidResource { .. } => {
                Self::invalid_input("register virtual device", error)
            }
            RegistryError::BusKindNotSupported { .. } | RegistryError::ArchNotSupported { .. } => {
                Self::unsupported("register virtual device", error)
            }
        }
    }
}

impl From<DeviceManagerError> for AxVmError {
    fn from(error: DeviceManagerError) -> Self {
        match error {
            DeviceManagerError::InvalidConfig { detail, .. } => Self::InvalidConfig { detail },
            DeviceManagerError::InvalidInput { operation, detail } => {
                Self::InvalidInput { operation, detail }
            }
            DeviceManagerError::ResourceNotFound {
                operation,
                resource,
            } => Self::resource_unavailable(
                "device resource",
                format_args!("{operation}: {resource}"),
            ),
            DeviceManagerError::ResourceConflict { operation, detail } => {
                Self::resource_conflict("device resource", format_args!("{operation}: {detail}"))
            }
            DeviceManagerError::OutOfMemory { operation } => Self::OutOfMemory { operation },
            DeviceManagerError::Unsupported { operation, detail } => {
                Self::Unsupported { operation, detail }
            }
            DeviceManagerError::Irq(error) => error.into(),
            DeviceManagerError::Registry(error) => error.into(),
            DeviceManagerError::Device(error) => error.into(),
            error => Self::device("manage virtual devices", error),
        }
    }
}

macro_rules! ax_err_type {
    (InvalidInput $(, $detail:expr)?) => {
        $crate::AxVmError::invalid_input(module_path!(), $crate::ax_err_type!(@detail $($detail)?))
    };
    (InvalidData $(, $detail:expr)?) => {
        $crate::AxVmError::invalid_config($crate::ax_err_type!(@detail $($detail)?))
    };
    (BadState $(, $detail:expr)?) => {
        $crate::AxVmError::invalid_state(module_path!(), $crate::ax_err_type!(@detail $($detail)?))
    };
    (NotFound $(, $detail:expr)?) => {
        $crate::AxVmError::resource_unavailable("requested resource", $crate::ax_err_type!(@detail $($detail)?))
    };
    (AlreadyExists $(, $detail:expr)?) => {
        $crate::AxVmError::resource_conflict("requested resource", $crate::ax_err_type!(@detail $($detail)?))
    };
    (AddrInUse $(, $detail:expr)?) => {
        $crate::AxVmError::resource_conflict("address range", $crate::ax_err_type!(@detail $($detail)?))
    };
    (ResourceBusy $(, $detail:expr)?) => {
        $crate::AxVmError::resource_conflict("requested resource", $crate::ax_err_type!(@detail $($detail)?))
    };
    (Unsupported $(, $detail:expr)?) => {
        $crate::AxVmError::unsupported(module_path!(), $crate::ax_err_type!(@detail $($detail)?))
    };
    (NoMemory $(, $detail:expr)?) => {
        $crate::AxVmError::OutOfMemory { operation: module_path!() }
    };
    (Io $(, $detail:expr)?) => {
        $crate::AxVmError::host(module_path!(), $crate::ax_err_type!(@detail $($detail)?))
    };
    (@detail $detail:expr) => { $detail };
    (@detail) => { "no additional detail" };
}

macro_rules! ax_err {
    ($kind:ident $(, $detail:expr)? $(,)?) => {
        Err($crate::ax_err_type!($kind $(, $detail)?))
    };
}

pub(crate) use ax_err;
pub(crate) use ax_err_type;

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn domain_errors_preserve_operation_context() {
        let error = AxVmError::memory("map guest region", "address conflict");

        assert!(matches!(error, AxVmError::Memory { .. }));
        assert_eq!(
            error.to_string(),
            "VM memory operation map guest region failed: address conflict"
        );
    }

    #[test]
    fn lower_layer_failures_map_to_matching_domains() {
        let cases = [
            AxVmError::memory("map stage-2 page", "invalid address"),
            AxVmError::device("register UART", "address in use"),
            AxVmError::vcpu("create vCPU", "backend rejected setup"),
            AxVmError::interrupt("inject IRQ", "controller unavailable"),
            AxVmError::host("enable virtualization", "unsupported CPU"),
        ];

        assert!(matches!(cases[0], AxVmError::Memory { .. }));
        assert!(matches!(cases[1], AxVmError::Device { .. }));
        assert!(matches!(cases[2], AxVmError::Vcpu { .. }));
        assert!(matches!(cases[3], AxVmError::Interrupt { .. }));
        assert!(matches!(cases[4], AxVmError::Host { .. }));
        for error in cases {
            let display = error.to_string();
            assert!(display.contains("operation"));
            assert!(display.contains("failed"));
        }
    }

    #[test]
    fn resource_and_capacity_errors_are_matchable() {
        let conflict = AxVmError::resource_conflict("guest address range", "already mapped");
        let exhausted = AxVmError::OutOfMemory {
            operation: "allocate guest RAM",
        };

        assert!(matches!(conflict, AxVmError::ResourceConflict { .. }));
        assert_eq!(
            conflict.to_string(),
            "VM resource guest address range conflicts: already mapped"
        );
        assert_eq!(
            exhausted.to_string(),
            "out of memory while allocate guest RAM"
        );
    }

    #[test]
    fn address_space_errors_map_to_axvm_domains() {
        assert!(matches!(
            AxVmError::from_addrspace("map guest RAM", AddrSpaceError::MappingConflict),
            AxVmError::ResourceConflict {
                resource: "guest address range",
                ..
            }
        ));
        assert!(matches!(
            AxVmError::from_addrspace(
                "map guest RAM",
                AddrSpaceError::Unaligned {
                    subject: "mapping size",
                    value: 1,
                    alignment: 0x1000,
                },
            ),
            AxVmError::InvalidInput {
                operation: "map guest RAM",
                ..
            }
        ));
        assert!(matches!(
            AxVmError::from_addrspace("query guest RAM", AddrSpaceError::MappingState),
            AxVmError::Memory {
                operation: "query guest RAM",
                ..
            }
        ));
    }
}
