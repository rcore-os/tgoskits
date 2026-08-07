use alloc::{boxed::Box, format, string::String};

use fdt_raw::FdtError;

use crate::DeviceId;

#[derive(thiserror::Error, Debug)]
pub enum DriverError {
    #[error("FDT error: {0}")]
    Fdt(String),
    #[error("unsupported driver source: {0}")]
    Unsupported(&'static str),
    #[error("Unknown driver error: {0}")]
    Unknown(String),
}

/// Failure to prepare or publish an FDT child provider.
#[derive(thiserror::Error, Debug)]
pub enum FdtChildProviderError {
    #[error("FDT node {path} does not belong to the active device tree")]
    ForeignNode { path: String },
    #[error("FDT node {child_path} is not a direct child of {parent_path}")]
    NotDirectChild {
        parent_path: String,
        child_path: String,
    },
    #[error("FDT child provider {path} is disabled")]
    Disabled { path: String },
    #[error("device {device_id:?} has no FDT parent identity")]
    ParentHasNoFdtIdentity { device_id: DeviceId },
    #[error(
        "FDT child provider {path} belongs to parent {expected_parent:?}, not {actual_parent:?}"
    )]
    ParentMismatch {
        path: String,
        expected_parent: DeviceId,
        actual_parent: DeviceId,
    },
    #[error("FDT child provider {path} is already populated outside a child-provider owner")]
    AlreadyPopulated { path: String },
    #[error("FDT child provider {path} is owned by {owner:?}, not requesting parent {requester:?}")]
    OwnershipConflict {
        path: String,
        owner: DeviceId,
        requester: DeviceId,
    },
    #[error("device {path} already exposes capability {interface}")]
    DuplicateCapability {
        path: String,
        interface: &'static str,
    },
}

impl From<FdtError> for DriverError {
    fn from(value: FdtError) -> Self {
        Self::Fdt(format!("{value:?}"))
    }
}

impl From<Box<dyn core::error::Error>> for DriverError {
    fn from(value: Box<dyn core::error::Error>) -> Self {
        Self::Unknown(format!("{value:?}"))
    }
}
