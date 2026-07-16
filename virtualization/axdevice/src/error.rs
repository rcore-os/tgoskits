//! AxDevice-owned error contract.

use alloc::string::String;

use axdevice_base::{AccessWidth, BusKind, DeviceError, IrqError, RegistryError};

/// Result type returned by device manager operations.
pub type DeviceManagerResult<T = ()> = Result<T, DeviceManagerError>;

/// Errors reported while configuring, registering, or accessing VM devices.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeviceManagerError {
    /// Device configuration is malformed or inconsistent.
    #[error("invalid device configuration for {operation}: {detail}")]
    InvalidConfig {
        /// The configuration operation that failed.
        operation: &'static str,
        /// Diagnostic detail describing the invalid configuration.
        detail: String,
    },
    /// An operation received an invalid argument.
    #[error("invalid input for device operation {operation}: {detail}")]
    InvalidInput {
        /// The operation that rejected the input.
        operation: &'static str,
        /// Diagnostic detail describing the invalid input.
        detail: String,
    },
    /// A required device resource was not found.
    #[error("device resource {resource} was not found during {operation}")]
    ResourceNotFound {
        /// The operation that required the resource.
        operation: &'static str,
        /// The missing resource.
        resource: String,
    },
    /// A device resource conflicts with an existing resource.
    #[error("device resource conflict during {operation}: {detail}")]
    ResourceConflict {
        /// The operation that discovered the conflict.
        operation: &'static str,
        /// Diagnostic detail describing both resources.
        detail: String,
    },
    /// A device allocation failed.
    #[error("out of memory during device operation {operation}")]
    OutOfMemory {
        /// The operation that attempted the allocation.
        operation: &'static str,
    },
    /// The requested device operation is unsupported.
    #[error("unsupported device operation {operation}: {detail}")]
    Unsupported {
        /// The unsupported operation.
        operation: &'static str,
        /// Diagnostic detail describing the limitation.
        detail: String,
    },
    /// A device returned a response that does not match the request.
    #[error("unexpected response during device operation {operation}: {detail}")]
    UnexpectedResponse {
        /// The operation that received the response.
        operation: &'static str,
        /// Diagnostic detail describing the response.
        detail: String,
    },
    /// An external device backend failed while servicing the guest model.
    #[error("device backend failed during {operation}: {detail}")]
    Backend {
        /// Backend operation that failed.
        operation: &'static str,
        /// Stable diagnostic detail supplied by the adapter.
        detail: String,
    },
    /// A bus access failed with address and width context.
    #[error("device {operation} failed on {bus:?} bus at {addr:#x} with width {width:?}: {source}")]
    Access {
        /// Whether the access is a read or write.
        operation: &'static str,
        /// The bus that received the access.
        bus: BusKind,
        /// The raw bus address.
        addr: u64,
        /// The requested access width.
        width: AccessWidth,
        /// The low-level device failure.
        #[source]
        source: DeviceError,
    },
    /// A low-level device access failed.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// Device registration or resource validation failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// IRQ resolution or signaling failed.
    #[error(transparent)]
    Irq(#[from] IrqError),
}

impl From<DeviceManagerError> for DeviceError {
    fn from(error: DeviceManagerError) -> Self {
        match error {
            DeviceManagerError::Device(error) => error,
            DeviceManagerError::OutOfMemory { operation } => Self::OutOfMemory { operation },
            DeviceManagerError::Unsupported { operation, detail } => {
                Self::Unsupported { operation, detail }
            }
            DeviceManagerError::InvalidConfig { operation, detail } => {
                Self::InvalidData { operation, detail }
            }
            DeviceManagerError::InvalidInput { operation, detail } => {
                Self::InvalidInput { operation, detail }
            }
            DeviceManagerError::ResourceNotFound {
                operation,
                resource,
            } => Self::InvalidState {
                operation,
                detail: alloc::format!("resource {resource} was not found"),
            },
            DeviceManagerError::ResourceConflict { operation, detail } => Self::ResourceBusy {
                operation,
                resource: detail,
            },
            DeviceManagerError::UnexpectedResponse { operation, detail } => {
                Self::InvalidState { operation, detail }
            }
            DeviceManagerError::Backend { operation, detail } => {
                Self::Backend { operation, detail }
            }
            DeviceManagerError::Access { source, .. } => source,
            DeviceManagerError::Registry(error) => Self::InvalidInput {
                operation: "register device",
                detail: alloc::format!("{error}"),
            },
            DeviceManagerError::Irq(error) => Self::Backend {
                operation: "route device IRQ",
                detail: alloc::format!("{error}"),
            },
        }
    }
}
